//! Interactive results browser (`--tui`).
//!
//! The scan streams results in live over a channel while you browse — scroll,
//! search, filter by verdict, drill into details, open/copy URLs. The scan
//! runs as a background task; the (sync) event loop polls the keyboard and
//! drains the channel, so the two stay decoupled.
//!
//! Testability: [`App`] (state transitions) and [`render`] are pure over a
//! fixed outcome set and are unit-tested with ratatui's `TestBackend`. The
//! [`run_live`] event loop — raw-mode setup, channel draining, `event::poll`,
//! teardown — is the thin shell that can only be exercised in a real terminal.

use std::io;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use adler_core::{CheckOutcome, MatchKind};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, List, ListItem, ListState, Padding, Paragraph, Wrap,
};

/// Accent colour used for titles, keys, and selection.
const ACCENT: Color = Color::Cyan;
/// Border colour. An explicit fixed grey (not `DIM`): terminals render the
/// `DIM` modifier inconsistently on box-drawing chars, which made parts of a
/// frame look brighter than others. A solid colour draws uniformly, and
/// dark-grey reads fine on both light and dark backgrounds.
const BORDER: Color = Color::DarkGray;
/// Muted style for secondary *text* (urls, labels, hints). `DIM` is relative
/// to the terminal's own foreground, so it stays readable on light *or* dark
/// themes — unlike a hardcoded grey. Borders use [`BORDER`], not this.
fn muted() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// A rounded, padded panel with an accent-coloured title, inset from the
/// corner by a short border stroke (`╭─ title ─╮`). The stroke matches the
/// border colour so the whole frame is one uniform shade.
fn panel(title: &str) -> Block<'_> {
    let border = Style::default().fg(BORDER);
    let title = Line::from(vec![
        Span::styled("─ ", border),
        Span::styled(
            title.to_owned(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ", border),
    ]);
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(title)
        .padding(Padding::horizontal(1))
}

/// Verdict filter cycled with `f`. `Relevant` (found + uncertain) is the
/// default — it hides the usually-large pile of `NotFound` rows, matching
/// the text output's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filter {
    Relevant,
    All,
    Found,
    NotFound,
    Uncertain,
}

impl Filter {
    fn label(self) -> &'static str {
        match self {
            Self::Relevant => "found+uncertain",
            Self::All => "all",
            Self::Found => "found",
            Self::NotFound => "not found",
            Self::Uncertain => "uncertain",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Relevant => Self::All,
            Self::All => Self::Found,
            Self::Found => Self::NotFound,
            Self::NotFound => Self::Uncertain,
            Self::Uncertain => Self::Relevant,
        }
    }

    fn matches(self, kind: MatchKind) -> bool {
        match self {
            Self::Relevant => kind != MatchKind::NotFound,
            Self::All => true,
            Self::Found => kind == MatchKind::Found,
            Self::NotFound => kind == MatchKind::NotFound,
            Self::Uncertain => kind == MatchKind::Uncertain,
        }
    }
}

/// A side effect the event loop should perform after a keypress. Returned
/// from [`App::handle_key`] so that [`App`] itself stays pure and testable —
/// the actual browser launch / clipboard write happens in [`run_live`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// Open this URL in the system browser.
    Open(String),
    /// Copy this URL to the clipboard.
    Copy(String),
    /// Copy all found-account URLs (newline-joined) to the clipboard.
    CopyAllFound(String),
}

/// Rows to jump for PageUp/PageDown.
const PAGE: usize = 10;
/// Minimum body width to show the persistent list+detail split; narrower
/// terminals stay single-pane (detail via `Enter`).
const SPLIT_MIN_WIDTH: u16 = 90;

/// Browser state over a fixed set of scan outcomes.
// UI mode flags are naturally bool-heavy; the pedantic lint doesn't apply.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct App {
    outcomes: Vec<CheckOutcome>,
    filter: Filter,
    /// Substring search over site/url (case-insensitive). Empty = no filter.
    search: String,
    /// Whether keypresses are currently editing the search query.
    searching: bool,
    selected: usize,
    detail: bool,
    quit: bool,
    /// Whether the `?` help overlay is open.
    help: bool,
    /// True while the scan is still feeding in results (live mode).
    scanning: bool,
    /// Transient feedback from the last action (open/copy), shown in the
    /// footer until the next keypress. `None` = show the help line.
    status: Option<String>,
}

impl App {
    /// Build an app, sorting outcomes by site for stable display.
    pub(crate) fn new(mut outcomes: Vec<CheckOutcome>) -> Self {
        outcomes.sort_by(|a, b| a.site.cmp(&b.site));
        Self {
            outcomes,
            filter: Filter::Relevant,
            search: String::new(),
            searching: false,
            selected: 0,
            detail: false,
            quit: false,
            help: false,
            scanning: false,
            status: None,
        }
    }

    /// Build an empty app for live streaming; results arrive via [`Self::push`].
    pub(crate) fn live() -> Self {
        let mut app = Self::new(Vec::new());
        app.scanning = true;
        app
    }

    /// Append an outcome, keeping the list sorted by site name (live mode).
    pub(crate) fn push(&mut self, outcome: CheckOutcome) {
        let pos = self.outcomes.partition_point(|x| x.site < outcome.site);
        self.outcomes.insert(pos, outcome);
    }

    /// Mark the scan complete (clears the "scanning…" indicator).
    pub(crate) fn finish_scanning(&mut self) {
        self.scanning = false;
    }

    /// Record transient footer feedback (set by the event loop after an
    /// open/copy action).
    pub(crate) fn set_status(&mut self, message: String) {
        self.status = Some(message);
    }

    fn visible(&self) -> Vec<&CheckOutcome> {
        let needle = self.search.to_lowercase();
        self.outcomes
            .iter()
            .filter(|o| self.filter.matches(o.kind))
            .filter(|o| {
                needle.is_empty()
                    || o.site.to_lowercase().contains(&needle)
                    || o.url.to_lowercase().contains(&needle)
            })
            .collect()
    }

    fn selected_outcome(&self) -> Option<&CheckOutcome> {
        self.visible().get(self.selected).copied()
    }

    fn next(&mut self) {
        let n = self.visible().len();
        if n > 0 {
            self.selected = (self.selected + 1) % n;
        }
    }

    fn prev(&mut self) {
        let n = self.visible().len();
        if n > 0 {
            self.selected = (self.selected + n - 1) % n;
        }
    }

    /// Clamp selection to the last visible row (after a page/end jump).
    fn last_index(&self) -> usize {
        self.visible().len().saturating_sub(1)
    }

    fn page_down(&mut self) {
        self.selected = (self.selected + PAGE).min(self.last_index());
    }

    fn page_up(&mut self) {
        self.selected = self.selected.saturating_sub(PAGE);
    }

    fn cycle_filter(&mut self) {
        self.filter = self.filter.next();
        self.selected = 0;
    }

    /// Move the selection to the next (`forward`) / previous found account in
    /// the visible list, wrapping. No-op if nothing is visible.
    fn jump_found(&mut self, forward: bool) {
        let visible = self.visible();
        let n = visible.len();
        if n == 0 {
            return;
        }
        let start = self.selected.min(n - 1);
        for step in 1..=n {
            let idx = if forward {
                (start + step) % n
            } else {
                (start + n - step) % n
            };
            if visible[idx].kind.is_found() {
                self.selected = idx;
                return;
            }
        }
    }

    /// URLs of all `Found` accounts, newline-joined (for `Y` bulk copy).
    fn found_urls(&self) -> String {
        let mut urls: Vec<&str> = self
            .outcomes
            .iter()
            .filter(|o| o.kind == MatchKind::Found)
            .map(|o| o.url.as_str())
            .collect();
        urls.sort_unstable();
        urls.join("\n")
    }

    fn toggle_detail(&mut self) {
        if self.selected_outcome().is_some() {
            self.detail = !self.detail;
        }
    }

    /// Apply a keypress. Returns an [`Action`] when the key requests external
    /// I/O (open/copy). Public for testing the event mapping.
    pub(crate) fn handle_key(&mut self, code: KeyCode) -> Option<Action> {
        // Any keypress clears the previous action's transient status; an
        // open/copy key sets a fresh one (via the event loop calling
        // `set_status` after performing the returned Action).
        self.status = None;
        if self.searching {
            self.handle_search_key(code);
            return None;
        }
        // While the help overlay is open, any key just closes it.
        if self.help {
            self.help = false;
            return None;
        }
        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.detail {
                    self.detail = false;
                } else {
                    self.quit = true;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.next(),
            KeyCode::Up | KeyCode::Char('k') => self.prev(),
            KeyCode::Char('g') | KeyCode::Home => self.selected = 0,
            KeyCode::Char('G') | KeyCode::End => self.selected = self.last_index(),
            KeyCode::PageDown => self.page_down(),
            KeyCode::PageUp => self.page_up(),
            // Hidden power keys: jump between found accounts (the green dots).
            KeyCode::Char('n') => self.jump_found(true),
            KeyCode::Char('N') => self.jump_found(false),
            KeyCode::Char('f') => self.cycle_filter(),
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('/') => {
                self.searching = true;
                self.search.clear();
                self.selected = 0;
            }
            KeyCode::Char('o') => {
                return self.selected_outcome().map(|o| Action::Open(o.url.clone()));
            }
            KeyCode::Char('y') => {
                return self.selected_outcome().map(|o| Action::Copy(o.url.clone()));
            }
            KeyCode::Char('Y') => {
                let urls = self.found_urls();
                if !urls.is_empty() {
                    return Some(Action::CopyAllFound(urls));
                }
            }
            KeyCode::Enter => self.toggle_detail(),
            _ => {}
        }
        None
    }

    fn handle_search_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.searching = false;
                self.search.clear();
                self.selected = 0;
            }
            KeyCode::Enter => self.searching = false,
            KeyCode::Backspace => {
                self.search.pop();
                self.selected = 0;
            }
            KeyCode::Char(c) => {
                self.search.push(c);
                self.selected = 0;
            }
            _ => {}
        }
    }

    /// Whether the event loop should exit.
    pub(crate) fn should_quit(&self) -> bool {
        self.quit
    }
}

fn kind_style(kind: MatchKind) -> (&'static str, Style) {
    match kind {
        MatchKind::Found => (
            "●",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        MatchKind::NotFound => ("·", muted()),
        MatchKind::Uncertain => (
            "▲",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    }
}

/// Render the current frame from `app` state.
pub(crate) fn render(frame: &mut Frame<'_>, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(frame.area());

    let visible = app.visible();
    let (mut found, mut not_found, mut uncertain) = (0_usize, 0_usize, 0_usize);
    for o in &app.outcomes {
        match o.kind {
            MatchKind::Found => found += 1,
            MatchKind::NotFound => not_found += 1,
            MatchKind::Uncertain => uncertain += 1,
        }
    }
    let pos = if visible.is_empty() {
        0
    } else {
        app.selected.min(visible.len() - 1) + 1
    };
    let dim = muted();
    let mut header = vec![
        Span::styled(
            " adler ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("· ", dim),
        Span::styled(found.to_string(), Style::default().fg(Color::Green)),
        Span::styled(" found · ", dim),
        Span::styled(not_found.to_string(), dim),
        Span::styled(" not found · ", dim),
        Span::styled(uncertain.to_string(), Style::default().fg(Color::Yellow)),
        Span::styled(" uncertain · ", dim),
        Span::styled(
            format!("filter: {} · [{pos}/{}]", app.filter.label(), visible.len()),
            dim,
        ),
    ];
    if app.scanning {
        header.push(Span::styled(
            " · scanning…",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(header)), chunks[0]);

    let body = chunks[1];
    if app.help {
        render_help(frame, body);
    } else if app.detail {
        // Enter zooms the detail to the full body width (good for long bios).
        render_detail(frame, body, app.selected_outcome());
    } else if body.width >= SPLIT_MIN_WIDTH {
        // Wide terminal: persistent master-detail (list left, detail right).
        let panes = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(body);
        render_list(frame, panes[0], &visible, app.selected);
        render_detail(frame, panes[1], app.selected_outcome());
    } else {
        render_list(frame, body, &visible, app.selected);
    }

    let footer = if app.searching {
        Line::from(vec![
            Span::styled("/", Style::default().fg(ACCENT)),
            Span::raw(app.search.clone()),
            Span::styled("▏", Style::default().fg(ACCENT)), // cursor
        ])
    } else if let Some(status) = &app.status {
        Line::from(Span::styled(
            format!(" {status} "),
            Style::default().fg(ACCENT),
        ))
    } else if app.help {
        Line::from(Span::styled(" any key: close help ", muted()))
    } else if app.detail {
        hint_line(&[("q/Esc", "back")])
    } else {
        hint_line(&[
            ("j/k", "move"),
            ("Enter", "details"),
            ("/", "search"),
            ("f", "filter"),
            ("o", "open"),
            ("y/Y", "copy"),
            ("?", "help"),
            ("q", "quit"),
        ])
    };
    frame.render_widget(Paragraph::new(footer), chunks[2]);
}

/// Build a footer hint line: accent-coloured keys, muted descriptions.
fn hint_line(hints: &[(&str, &str)]) -> Line<'static> {
    let key = Style::default().fg(ACCENT);
    let dim = muted();
    let mut spans = vec![Span::raw(" ")];
    for (i, (k, desc)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(Span::styled((*k).to_string(), key));
        spans.push(Span::styled(format!(" {desc}"), dim));
    }
    Line::from(spans)
}

/// Full-area keybinding reference, shown on `?`.
fn render_help(frame: &mut Frame<'_>, area: Rect) {
    let keys = [
        ("j / k, ↑ / ↓", "move selection"),
        ("g / G, Home / End", "jump to top / bottom"),
        ("PageUp / PageDown", "page up / down"),
        ("n / N", "next / prev found account"),
        ("/", "incremental search (Esc cancels)"),
        ("f", "cycle verdict filter"),
        ("Enter", "toggle detail view"),
        ("o", "open selected URL in browser"),
        ("y", "copy selected URL"),
        ("Y", "copy all found URLs"),
        ("?", "toggle this help"),
        ("q / Esc", "back / quit"),
    ];
    let lines: Vec<Line<'_>> = keys
        .iter()
        .map(|(k, desc)| {
            Line::from(vec![
                Span::styled(
                    format!("{k:<20}"),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {desc}"), muted()),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(panel("keybindings")), area);
}

fn render_list(frame: &mut Frame<'_>, area: Rect, visible: &[&CheckOutcome], selected: usize) {
    let items: Vec<ListItem<'_>> = visible
        .iter()
        .map(|o| {
            let (sym, style) = kind_style(o.kind);
            // No URL here: in a narrow pane it would be truncated, and the
            // terminal's ctrl-click would then open the cut-off link. The full
            // URL lives in the detail pane (Enter / wide split) and `o`/`y`.
            ListItem::new(Line::from(vec![
                Span::styled(format!("{sym} "), style),
                Span::styled(o.site.clone(), muted()),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(panel("results"))
        // Bold + a leading bar instead of REVERSED: reversing a row turns each
        // coloured glyph's fg into a background block (the green dot + its
        // trailing space). Bold keeps glyph colours intact; the ▌ marks the row.
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("▌ ");
    let mut state = ListState::default();
    if !visible.is_empty() {
        state.select(Some(selected.min(visible.len() - 1)));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_detail(frame: &mut Frame<'_>, area: Rect, outcome: Option<&CheckOutcome>) {
    let label = muted();
    let section = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line<'_>> = Vec::new();
    if let Some(o) = outcome {
        let (sym, style) = kind_style(o.kind);
        lines.push(Line::from(vec![
            Span::styled(format!("{sym} "), style),
            Span::styled(
                o.site.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("url: ", label),
            Span::raw(o.url.clone()),
        ]));
        lines.push(Line::from(vec![
            Span::styled("elapsed: ", label),
            Span::raw(format!("{} ms", o.elapsed_ms)),
        ]));
        if let Some(reason) = &o.reason {
            lines.push(Line::from(vec![
                Span::styled("note: ", label),
                Span::raw(reason.to_string()),
            ]));
        }
        if !o.evidence.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("why:", section)));
            for line in &o.evidence {
                lines.push(Line::from(format!("  {line}")));
            }
        }
        if !o.enrichment.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("profile:", section)));
            for (k, v) in &o.enrichment {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {k}: "), label),
                    Span::raw(v.clone()),
                ]));
            }
        }
    } else {
        lines.push(Line::from(Span::styled("no selection", label)));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("details"))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// Run the interactive browser while a scan streams results in over `rx`.
///
/// Polls the keyboard with a short timeout so the frame keeps refreshing as
/// outcomes arrive; appends each received outcome to the live list. When the
/// sender is dropped (scan finished or aborted) the "scanning…" indicator
/// clears. Blocks until the user quits.
pub(crate) fn run_live(rx: &Receiver<CheckOutcome>) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let mut app = App::live();
    let result = loop {
        if let Err(err) = terminal.draw(|frame| render(frame, &app)) {
            break Err(err);
        }
        // Drain everything available without blocking.
        loop {
            match rx.try_recv() {
                Ok(outcome) => app.push(outcome),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    app.finish_scanning();
                    break;
                }
            }
        }
        match event::poll(Duration::from_millis(120)) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    if let Some(action) = app.handle_key(key.code) {
                        app.set_status(perform(&action));
                    }
                    if app.should_quit() {
                        break Ok(());
                    }
                }
                Ok(_) => {}
                Err(err) => break Err(err),
            },
            Ok(false) => {}
            Err(err) => break Err(err),
        }
    };
    ratatui::restore();
    result
}

/// Carry out an [`Action`]'s side effect and return footer feedback.
///
/// Failures are non-fatal: a missing browser or unsupported clipboard
/// shouldn't crash the session — but, unlike before, they're now reported in
/// the footer so the user isn't left wondering whether anything happened
/// (e.g. `o` on a headless / SSH host with no `xdg-open` handler).
fn perform(action: &Action) -> String {
    match action {
        Action::Open(url) => match open::that(url) {
            Ok(()) => format!("opened {url}"),
            Err(err) => format!("couldn't open browser ({err}); copy with y instead"),
        },
        Action::Copy(url) => {
            copy_to_clipboard(url);
            format!("copied {url} (OSC 52 — needs a supporting terminal)")
        }
        Action::CopyAllFound(urls) => {
            copy_to_clipboard(urls);
            let n = urls.lines().count();
            format!("copied {n} found URL(s) (OSC 52 — needs a supporting terminal)")
        }
    }
}

/// Copy `text` to the clipboard via the OSC 52 terminal escape — no GUI
/// clipboard dependency, and it works over SSH in terminals that support it
/// (`kitty`, `iTerm2`, `WezTerm`, `tmux` with `set-clipboard on`).
fn copy_to_clipboard(text: &str) {
    use std::io::Write as _;
    let seq = format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()));
    let mut stdout = io::stdout();
    let _ = stdout.write_all(seq.as_bytes());
    let _ = stdout.flush();
}

/// Minimal standard base64 encoder (avoids pulling in a crate for one use).
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0b11) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(((b1 & 0b1111) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(b2 & 0b11_1111) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::collections::BTreeMap;

    fn outcome(site: &str, kind: MatchKind) -> CheckOutcome {
        CheckOutcome {
            site: site.into(),
            url: format!("https://{site}.example/u"),
            kind,
            reason: None,
            elapsed_ms: 7,
            enrichment: BTreeMap::new(),
            evidence: Vec::new(),
        }
    }

    fn sample() -> Vec<CheckOutcome> {
        vec![
            outcome("GitHub", MatchKind::Found),
            outcome("GitLab", MatchKind::NotFound),
            outcome("Reddit", MatchKind::Uncertain),
        ]
    }

    fn buffer_text_sized(app: &App, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        buf.content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    // Default 80-wide backend stays single-pane (below SPLIT_MIN_WIDTH).
    fn buffer_text(app: &App) -> String {
        buffer_text_sized(app, 80, 12)
    }

    #[test]
    fn new_sorts_outcomes_by_site() {
        let app = App::new(vec![
            outcome("Zeta", MatchKind::Found),
            outcome("Alpha", MatchKind::Found),
        ]);
        assert_eq!(app.outcomes[0].site, "Alpha");
    }

    #[test]
    fn default_filter_is_relevant_hiding_not_found() {
        let app = App::new(sample());
        assert_eq!(app.filter, Filter::Relevant);
        // Found + Uncertain visible; NotFound (GitLab) hidden.
        let sites: Vec<&str> = app.visible().iter().map(|o| o.site.as_str()).collect();
        assert_eq!(sites, ["GitHub", "Reddit"]);
    }

    #[test]
    fn filter_cycles_through_all_modes() {
        let mut app = App::new(sample());
        app.cycle_filter(); // Relevant → All
        assert_eq!(app.filter, Filter::All);
        assert_eq!(app.visible().len(), 3);
        app.cycle_filter(); // All → Found
        assert_eq!(app.filter, Filter::Found);
        assert_eq!(app.visible().len(), 1);
        assert_eq!(app.visible()[0].site, "GitHub");
    }

    #[test]
    fn navigation_wraps_within_visible() {
        let mut app = App::new(sample()); // Relevant → 2 visible
        assert_eq!(app.selected, 0);
        app.prev(); // wraps to last (index 1)
        assert_eq!(app.selected, 1);
        app.next(); // wraps back to 0
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn paging_and_jumps_clamp_to_bounds() {
        let mut app = App::new(sample());
        app.cycle_filter(); // All → 3 visible
        app.page_down(); // PAGE > len → clamps to last
        assert_eq!(app.selected, 2);
        app.page_up();
        assert_eq!(app.selected, 0);
        app.selected = app.last_index();
        assert_eq!(app.selected, 2);
    }

    #[test]
    fn jump_found_moves_between_found_accounts() {
        // Reddit is Uncertain; only GitHub is Found among the visible two.
        let mut app = App::new(sample());
        app.selected = 1; // on Reddit (uncertain)
        app.handle_key(KeyCode::Char('n')); // jump to next found
        assert_eq!(app.visible()[app.selected].site, "GitHub");
    }

    #[test]
    fn copy_all_found_collects_found_urls() {
        let mut app = App::new(sample());
        let action = app.handle_key(KeyCode::Char('Y'));
        assert_eq!(
            action,
            Some(Action::CopyAllFound("https://GitHub.example/u".into()))
        );
    }

    #[test]
    fn question_mark_toggles_help_overlay() {
        let mut app = App::new(sample());
        app.handle_key(KeyCode::Char('?'));
        assert!(app.help);
        let text = buffer_text(&app);
        assert!(text.contains("keybindings"), "{text}");
        app.handle_key(KeyCode::Char('j')); // any key closes help
        assert!(!app.help);
    }

    #[test]
    fn enter_toggles_detail_when_selection_exists() {
        let mut app = App::new(sample());
        app.handle_key(KeyCode::Enter);
        assert!(app.detail);
        app.handle_key(KeyCode::Esc);
        assert!(!app.detail);
    }

    #[test]
    fn q_quits_from_list_but_closes_detail_first() {
        let mut app = App::new(sample());
        app.handle_key(KeyCode::Enter);
        app.handle_key(KeyCode::Char('q')); // closes detail
        assert!(app.detail.eq(&false));
        assert!(!app.should_quit());
        app.handle_key(KeyCode::Char('q')); // now quits
        assert!(app.should_quit());
    }

    #[test]
    fn renders_list_with_sites_and_header() {
        let app = App::new(sample());
        let text = buffer_text(&app);
        assert!(text.contains("GitHub"), "{text}");
        assert!(text.contains("filter: found+uncertain"), "{text}");
        assert!(
            text.contains("1 found · 1 not found · 1 uncertain"),
            "{text}"
        );
        // NotFound row is hidden by the default Relevant filter.
        assert!(!text.contains("GitLab"), "{text}");
    }

    #[test]
    fn wide_terminal_shows_persistent_detail_pane() {
        // Selection 0 = GitHub (sorted, found, visible under Relevant). On a
        // wide terminal the detail pane renders without pressing Enter.
        let app = App::new(sample());
        let text = buffer_text_sized(&app, 120, 12);
        assert!(text.contains("results"), "list pane present: {text}");
        assert!(text.contains("details"), "detail pane present: {text}");
        assert!(
            text.contains("elapsed: 7 ms"),
            "detail content present: {text}"
        );
    }

    #[test]
    fn narrow_terminal_is_single_pane() {
        // 80 < SPLIT_MIN_WIDTH: no detail pane until Enter. Check for the
        // detail pane's *content* (the footer hint mentions "details").
        let app = App::new(sample());
        let text = buffer_text_sized(&app, 80, 12);
        assert!(text.contains("results"), "{text}");
        assert!(
            !text.contains("elapsed:"),
            "no detail pane content when narrow: {text}"
        );
    }

    #[test]
    fn renders_detail_view() {
        let mut app = App::new(sample());
        app.handle_key(KeyCode::Enter);
        let text = buffer_text(&app);
        assert!(text.contains("details"));
        assert!(text.contains("elapsed: 7 ms"));
    }

    #[test]
    fn slash_enters_search_and_typing_narrows() {
        let mut app = App::new(sample());
        app.cycle_filter(); // Relevant → All, so NotFound GitLab is visible too
        app.handle_key(KeyCode::Char('/'));
        assert!(app.searching);
        for c in "git".chars() {
            app.handle_key(KeyCode::Char(c));
        }
        // "git" matches GitHub and GitLab, not Reddit.
        let sites: Vec<&str> = app.visible().iter().map(|o| o.site.as_str()).collect();
        assert_eq!(sites, ["GitHub", "GitLab"], "{sites:?}");
    }

    #[test]
    fn esc_cancels_search_and_clears_query() {
        let mut app = App::new(sample());
        app.handle_key(KeyCode::Char('/'));
        app.handle_key(KeyCode::Char('z'));
        assert_eq!(app.visible().len(), 0);
        app.handle_key(KeyCode::Esc);
        assert!(!app.searching);
        assert!(app.search.is_empty());
        // Back to the default Relevant view (found + uncertain).
        assert_eq!(app.visible().len(), 2);
    }

    #[test]
    fn q_typed_into_search_does_not_quit() {
        let mut app = App::new(sample());
        app.handle_key(KeyCode::Char('/'));
        app.handle_key(KeyCode::Char('q'));
        assert!(!app.should_quit());
        assert_eq!(app.search, "q");
    }

    #[test]
    fn o_and_y_return_actions_for_selection() {
        let mut app = App::new(sample());
        // Selection 0 is GitHub (sorted).
        assert_eq!(
            app.handle_key(KeyCode::Char('o')),
            Some(Action::Open("https://GitHub.example/u".into()))
        );
        assert_eq!(
            app.handle_key(KeyCode::Char('y')),
            Some(Action::Copy("https://GitHub.example/u".into()))
        );
    }

    #[test]
    fn live_app_pushes_in_sorted_order_and_tracks_scanning() {
        let mut app = App::live();
        assert!(app.scanning);
        app.push(outcome("Reddit", MatchKind::Found));
        app.push(outcome("GitHub", MatchKind::Found));
        app.push(outcome("Medium", MatchKind::Uncertain));
        let sites: Vec<&str> = app.outcomes.iter().map(|o| o.site.as_str()).collect();
        assert_eq!(sites, ["GitHub", "Medium", "Reddit"], "push keeps sorted");
        // Scanning indicator shows while live (wide buffer so the long header
        // isn't truncated before the marker).
        assert!(
            buffer_text_sized(&app, 120, 12).contains("scanning"),
            "header shows scanning"
        );
        app.finish_scanning();
        assert!(!app.scanning);
        assert!(!buffer_text_sized(&app, 120, 12).contains("scanning"));
    }

    #[test]
    fn status_shows_in_footer() {
        let mut app = App::new(sample());
        app.set_status("couldn't open browser".into());
        let text = buffer_text(&app);
        assert!(text.contains("couldn't open browser"), "{text}");
    }

    #[test]
    fn any_keypress_clears_prior_status() {
        let mut app = App::new(sample());
        app.set_status("opened https://x".into());
        assert!(app.status.is_some());
        app.handle_key(KeyCode::Char('j'));
        assert!(app.status.is_none(), "status should clear on next key");
    }

    #[test]
    fn base64_encode_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }
}
