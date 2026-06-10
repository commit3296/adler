//! Rendering helpers for scan outcomes.
//!
//! Hosts the formatters for every `--format` value (text, JSON,
//! NDJSON, CSV, HTML), the per-row printers used by the streaming
//! text path, and the cross-account correlation summary. Pure in its
//! inputs (writer + outcomes + opts) — no stdout locking or terminal
//! probing here, so every formatter is unit-testable against an
//! in-memory buffer.

use std::io::{self, Write};
use std::time::Duration;

use adler_core::{CheckOutcome, ConfidenceLabel, ConfidenceReason, CorrelationReport, MatchKind};
use anyhow::{Context as _, Result};
use indicatif::{ProgressBar, ProgressStyle};

use crate::OutputFormat;
use crate::report;

pub(crate) fn make_progress_bar(total: u64) -> ProgressBar {
    let bar = ProgressBar::new(total);
    let style = ProgressStyle::default_bar()
        .template("{spinner:.cyan} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len}")
        .unwrap_or_else(|_| ProgressStyle::default_bar());
    bar.set_style(style.progress_chars("=> "));
    bar
}

/// Whether any outcome is a positive hit. Drives the process exit code
/// (0 when true, 1 when false). `ExitCode` isn't comparable, so the testable
/// unit is this predicate.
pub(crate) fn any_found(outcomes: &[CheckOutcome]) -> bool {
    outcomes.iter().any(|o| o.kind.is_found())
}

/// What and how to print each text result row.
// Display toggles are naturally bool-heavy; the pedantic lint doesn't apply.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy)]
pub(crate) struct DisplayOpts {
    /// Show `NotFound` rows too (default hides the bulk noise).
    pub(crate) show_all: bool,
    /// Print only found URLs, no chrome (`--quiet`).
    pub(crate) quiet: bool,
    /// Colorize rows.
    pub(crate) color: bool,
    /// Print the signal evidence under each row (`--explain`).
    pub(crate) explain: bool,
}

/// Presentation options for [`write_outputs`].
pub(crate) struct OutputOpts<'a> {
    pub(crate) format: OutputFormat,
    pub(crate) display: DisplayOpts,
    pub(crate) username: &'a str,
    pub(crate) elapsed: Duration,
}

/// Whether a verdict should appear in human output. `Found` and `Uncertain`
/// are always shown; `NotFound` is the bulk noise, hidden unless `show_all`.
pub(crate) fn should_show(kind: MatchKind, show_all: bool) -> bool {
    show_all || kind != MatchKind::NotFound
}

/// Print one result row. In quiet mode only `Found` rows print, as a bare URL.
pub(crate) fn print_row(
    out: &mut impl Write,
    o: &CheckOutcome,
    disp: DisplayOpts,
) -> io::Result<()> {
    if disp.quiet {
        if o.kind == MatchKind::Found {
            writeln!(out, "{}", o.url)?;
        }
        return Ok(());
    }
    let (symbol, code) = match o.kind {
        MatchKind::Found => ("[+]", "\x1b[32m"),
        MatchKind::NotFound => ("[-]", "\x1b[2m"),
        MatchKind::Uncertain => ("[?]", "\x1b[33m"),
    };
    if disp.color {
        writeln!(out, "{code}{symbol}\x1b[0m {:<14} {}", o.site, o.url)?;
    } else {
        writeln!(out, "{symbol} {:<14} {}", o.site, o.url)?;
    }
    if let Some(reason) = &o.reason {
        writeln!(out, "    note: {reason}")?;
    }
    if let Some(confidence) = confidence_text(o) {
        writeln!(out, "    confidence: {confidence}")?;
    }
    if disp.explain {
        for reason in &o.confidence.reasons {
            writeln!(out, "    confidence-why: {}", confidence_reason(reason))?;
        }
        for line in &o.evidence {
            writeln!(out, "    why: {line}")?;
        }
        for ev in &o.profile_evidence {
            writeln!(
                out,
                "    profile: {}{} = {}",
                ev.kind.kind_label(),
                ev.field
                    .as_ref()
                    .map_or_else(String::new, |field| format!(" ({field})")),
                ev.value
            )?;
        }
    }
    for (field, value) in &o.enrichment {
        writeln!(out, "    {field}: {value}")?;
    }
    Ok(())
}

fn confidence_text(o: &CheckOutcome) -> Option<String> {
    if o.confidence.score == 0 && o.confidence.reasons.is_empty() {
        return None;
    }
    Some(format!(
        "{} {}%",
        confidence_label(o.confidence.label),
        o.confidence.score
    ))
}

fn confidence_label(label: ConfidenceLabel) -> &'static str {
    match label {
        ConfidenceLabel::Low => "low",
        ConfidenceLabel::Medium => "medium",
        ConfidenceLabel::High => "high",
        ConfidenceLabel::Verified => "verified",
    }
}

fn confidence_reason(reason: &ConfidenceReason) -> String {
    match reason {
        ConfidenceReason::FoundBySignal => "found by detection signal".to_owned(),
        ConfidenceReason::NotFoundBySignal => "not found by detection signal".to_owned(),
        ConfidenceReason::ProfileMetadataExtracted { count } => {
            format!("{count} profile metadata field(s) extracted")
        }
        ConfidenceReason::ProfileMetadataRich { count } => {
            format!("{count} rich profile metadata field(s) extracted")
        }
        ConfidenceReason::SignalEvidence { count } => {
            format!("{count} signal evidence line(s) recorded")
        }
        ConfidenceReason::AuthenticatedAccess => "authenticated access path used".to_owned(),
        ConfidenceReason::BrowserTransport => "browser transport produced verdict".to_owned(),
        ConfidenceReason::ImpersonateTransport => {
            "impersonating transport produced verdict".to_owned()
        }
        ConfidenceReason::EscalatedTransport => "escalated transport produced verdict".to_owned(),
        ConfidenceReason::WeakStatusOnly => {
            "weak status-only signal without supporting evidence".to_owned()
        }
        ConfidenceReason::UncertainOutcome => "uncertain outcome".to_owned(),
        ConfidenceReason::SessionRequired => "operator session required".to_owned(),
        ConfidenceReason::TransportBlocked => {
            "transport/access blocked reliable probing".to_owned()
        }
    }
}

trait ProfileEvidenceLabel {
    fn kind_label(&self) -> &'static str;
}

impl ProfileEvidenceLabel for adler_core::ProfileEvidenceKind {
    fn kind_label(&self) -> &'static str {
        match self {
            Self::DisplayName => "display_name",
            Self::Bio => "bio",
            Self::AvatarUrl => "avatar_url",
            Self::ExternalLink => "external_link",
            Self::Location => "location",
            Self::JoinedDate => "joined_date",
            Self::ProfileTitle => "profile_title",
            Self::MetaDescription => "meta_description",
            Self::ExtractedField => "extracted_field",
        }
    }
}

/// Print the final tally line, counted over *all* outcomes regardless of
/// what was displayed.
pub(crate) fn print_tally(
    out: &mut impl Write,
    outcomes: &[CheckOutcome],
    elapsed: Duration,
) -> io::Result<()> {
    let mut found = 0_usize;
    let mut not_found = 0_usize;
    let mut uncertain = 0_usize;
    for o in outcomes {
        match o.kind {
            MatchKind::Found => found += 1,
            MatchKind::NotFound => not_found += 1,
            MatchKind::Uncertain => uncertain += 1,
        }
    }
    writeln!(out)?;
    writeln!(
        out,
        "{found} found · {not_found} not found · {uncertain} uncertain · {:.2}s",
        elapsed.as_secs_f64()
    )
}

/// One-line suggestion of next steps, shown after an interactive text scan.
///
/// `enrich` / `correlate` come from the CLI struct; mirrored explicitly
/// rather than borrowing `&Cli` so this module stays independent of
/// the top-level parser.
pub(crate) fn print_hint(
    out: &mut impl Write,
    enrich: bool,
    correlate: bool,
    color: bool,
) -> io::Result<()> {
    let mut tips: Vec<&str> = Vec::new();
    if !enrich && !correlate {
        tips.push("--enrich for profiles");
    }
    tips.push("--format json to script");
    let line = format!("tip: {}", tips.join(" · "));
    if color {
        writeln!(out, "\x1b[2m{line}\x1b[0m")
    } else {
        writeln!(out, "{line}")
    }
}

/// Print one result row to stdout (used by the live streaming callback).
pub(crate) fn stream_row(o: &CheckOutcome, disp: DisplayOpts) {
    if should_show(o.kind, disp.show_all) {
        let mut out = io::stdout().lock();
        let _ = print_row(&mut out, o, disp);
    }
}

/// Stable lowercase label for a verdict (used in CSV; matches the JSON tag).
fn kind_label(kind: MatchKind) -> &'static str {
    match kind {
        MatchKind::Found => "found",
        MatchKind::NotFound => "not_found",
        MatchKind::Uncertain => "uncertain",
    }
}

/// Quote a CSV field per RFC 4180: wrap in double quotes and double any
/// internal quote when it contains a comma, quote, or newline.
pub(crate) fn csv_escape(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_owned()
    }
}

/// Write one CSV record (escaped, comma-joined, CRLF-free fields).
pub(crate) fn write_csv_row(out: &mut impl Write, fields: &[String]) -> io::Result<()> {
    let escaped: Vec<String> = fields.iter().map(|f| csv_escape(f)).collect();
    writeln!(out, "{}", escaped.join(","))
}

/// The per-outcome CSV columns (after any leading `username` in batch mode).
pub(crate) fn outcome_csv_fields(o: &CheckOutcome) -> Vec<String> {
    vec![
        o.site.clone(),
        o.url.clone(),
        kind_label(o.kind).to_owned(),
        o.reason
            .as_ref()
            .map_or_else(String::new, ToString::to_string),
        o.elapsed_ms.to_string(),
        o.evidence.join("; "),
    ]
}

pub(crate) const CSV_COLUMNS: &str = "site,url,kind,reason,elapsed_ms,evidence";

/// Write the cross-account correlation summary (text format).
pub(crate) fn print_correlation(
    out: &mut impl Write,
    report: &CorrelationReport,
) -> io::Result<()> {
    writeln!(out, "\ncorrelation:")?;
    if report.clusters.is_empty() {
        writeln!(out, "  no cross-site links found")?;
    }
    for cluster in &report.clusters {
        write!(
            out,
            "  • {} — {:.0}% confidence",
            cluster.members.join(", "),
            cluster.confidence * 100.0,
        )?;
        if let Some(name) = &cluster.shared_name {
            write!(out, " (shared name: {name:?})")?;
        }
        writeln!(out)?;
    }
    if !report.unlinked.is_empty() {
        writeln!(
            out,
            "  unlinked (profile data, no match): {}",
            report.unlinked.join(", ")
        )?;
    }
    if !report.without_profile.is_empty() {
        writeln!(
            out,
            "  no profile data: {}",
            report.without_profile.join(", ")
        )?;
    }
    Ok(())
}

/// Render outcomes (and optional correlation) to `out` in the chosen format.
///
/// Pure in its inputs and the writer — no stdout locking or terminal probing
/// here, so it's unit-testable against an in-memory buffer. This is the batch
/// path (piped text, JSON, NDJSON, CSV, HTML); interactive text streams rows
/// live during the scan instead (see `run_scan`).
pub(crate) fn write_outputs(
    out: &mut impl Write,
    opts: &OutputOpts<'_>,
    outcomes: &[CheckOutcome],
    correlation: Option<&CorrelationReport>,
) -> Result<()> {
    match opts.format {
        OutputFormat::Text => {
            let mut sorted: Vec<&CheckOutcome> = outcomes.iter().collect();
            sorted.sort_by(|a, b| a.site.cmp(&b.site));
            for o in &sorted {
                if should_show(o.kind, opts.display.show_all) {
                    print_row(out, o, opts.display).context("writing text")?;
                }
            }
            if !opts.display.quiet {
                print_tally(out, outcomes, opts.elapsed).context("writing tally")?;
                if let Some(report) = correlation {
                    print_correlation(out, report).context("writing correlation")?;
                }
            }
            Ok(())
        }
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut *out, outcomes).context("writing JSON")?;
            writeln!(out).context("writing JSON newline")
        }
        OutputFormat::Ndjson => {
            for outcome in outcomes {
                serde_json::to_writer(&mut *out, outcome).context("writing NDJSON")?;
                writeln!(out).context("writing NDJSON newline")?;
            }
            Ok(())
        }
        OutputFormat::Csv => {
            writeln!(out, "{CSV_COLUMNS}").context("writing CSV header")?;
            let mut sorted: Vec<&CheckOutcome> = outcomes.iter().collect();
            sorted.sort_by(|a, b| a.site.cmp(&b.site));
            for o in &sorted {
                write_csv_row(out, &outcome_csv_fields(o)).context("writing CSV row")?;
            }
            Ok(())
        }
        OutputFormat::Html => {
            let html = report::render_html(opts.username, outcomes, correlation, opts.elapsed);
            out.write_all(html.as_bytes()).context("writing HTML")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adler_core::{CorrelationReport, UncertainReason};
    use std::collections::BTreeMap;

    fn outcome(site: &str, kind: MatchKind) -> CheckOutcome {
        CheckOutcome {
            site: site.into(),
            url: format!("https://{site}.example/u"),
            kind,
            reason: None,
            elapsed_ms: 1,
            enrichment: BTreeMap::new(),
            evidence: Vec::new(),
            profile_evidence: Vec::new(),
            confidence: adler_core::ConfidenceScore::default(),
            transport: None,
            escalations: 0,
        }
    }

    fn opts(format: OutputFormat, show_all: bool, quiet: bool) -> OutputOpts<'static> {
        OutputOpts {
            format,
            display: DisplayOpts {
                show_all,
                quiet,
                color: false,
                explain: false,
            },
            username: "alice",
            elapsed: Duration::from_secs(1),
        }
    }

    /// Render to an in-memory buffer (no stdout / no colour).
    fn render(format: OutputFormat, show_all: bool, outcomes: &[CheckOutcome]) -> String {
        let mut buf: Vec<u8> = Vec::new();
        write_outputs(&mut buf, &opts(format, show_all, false), outcomes, None).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn any_found_reflects_a_positive_hit() {
        assert!(any_found(&[outcome("A", MatchKind::Found)]));
        assert!(!any_found(&[
            outcome("A", MatchKind::NotFound),
            outcome("B", MatchKind::Uncertain),
        ]));
        assert!(!any_found(&[]));
    }

    #[test]
    fn csv_escape_quotes_only_when_needed() {
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_escape("line1\nline2"), "\"line1\nline2\"");
        assert_eq!(csv_escape(""), "");
    }

    #[test]
    fn should_show_hides_only_not_found_by_default() {
        assert!(should_show(MatchKind::Found, false));
        assert!(should_show(MatchKind::Uncertain, false));
        assert!(!should_show(MatchKind::NotFound, false));
        assert!(should_show(MatchKind::NotFound, true));
    }

    #[test]
    fn text_default_shows_found_and_uncertain_hides_not_found() {
        let outcomes = vec![
            outcome("GitHub", MatchKind::Found),
            outcome("GitLab", MatchKind::NotFound),
            outcome("Reddit", MatchKind::Uncertain),
        ];
        let text = render(OutputFormat::Text, false, &outcomes);
        assert!(text.contains("[+] GitHub"), "{text}");
        assert!(text.contains("[?] Reddit"), "{text}");
        assert!(!text.contains("[-] GitLab"), "not-found hidden by default");
        // Tally still counts everything.
        assert!(
            text.contains("1 found · 1 not found · 1 uncertain"),
            "{text}"
        );
    }

    #[test]
    fn text_all_shows_not_found_too() {
        let outcomes = vec![
            outcome("GitHub", MatchKind::Found),
            outcome("GitLab", MatchKind::NotFound),
        ];
        let text = render(OutputFormat::Text, true, &outcomes);
        assert!(text.contains("[+] GitHub"));
        assert!(text.contains("[-] GitLab"), "{text}");
    }

    #[test]
    fn quiet_prints_only_found_urls() {
        let outcomes = vec![
            outcome("GitHub", MatchKind::Found),
            outcome("GitLab", MatchKind::NotFound),
            outcome("Reddit", MatchKind::Uncertain),
        ];
        let mut buf: Vec<u8> = Vec::new();
        write_outputs(
            &mut buf,
            &opts(OutputFormat::Text, false, true),
            &outcomes,
            None,
        )
        .unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert_eq!(text, "https://GitHub.example/u\n", "{text:?}");
    }

    #[test]
    fn text_renders_reason_note() {
        let mut o = outcome("Site", MatchKind::Uncertain);
        o.reason = Some(UncertainReason::RateLimited);
        let text = render(OutputFormat::Text, false, &[o]);
        assert!(text.contains("note: rate_limited"), "{text}");
    }

    #[test]
    fn text_renders_confidence_when_available() {
        let mut o = outcome("Site", MatchKind::Found);
        o.evidence.push("HTTP 200 (status_found)".into());
        o.refresh_confidence();

        let text = render(OutputFormat::Text, false, &[o]);
        assert!(text.contains("confidence: medium 70%"), "{text}");
    }

    #[test]
    fn explain_renders_confidence_reasons_and_profile_evidence() {
        let mut o = outcome("Site", MatchKind::Found);
        o.evidence.push("HTTP 200 (status_found)".into());
        o.profile_evidence
            .push(adler_core::ProfileEvidence::from_enrichment(
                "Site",
                "https://Site.example/u",
                "name",
                "Alice",
            ));
        o.refresh_confidence();

        let mut buf: Vec<u8> = Vec::new();
        let mut opts = opts(OutputFormat::Text, false, false);
        opts.display.explain = true;
        write_outputs(&mut buf, &opts, &[o], None).unwrap();
        let text = String::from_utf8(buf).unwrap();

        assert!(
            text.contains("confidence-why: found by detection signal"),
            "{text}"
        );
        assert!(
            text.contains("profile: display_name (name) = Alice"),
            "{text}"
        );
    }

    #[test]
    fn json_output_is_an_array() {
        let outcomes = vec![outcome("GitHub", MatchKind::Found)];
        let json = render(OutputFormat::Json, false, &outcomes);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value.as_array().unwrap().len(), 1);
        assert_eq!(value[0]["kind"], "found");
    }

    #[test]
    fn ndjson_output_is_one_object_per_line() {
        let outcomes = vec![
            outcome("A", MatchKind::Found),
            outcome("B", MatchKind::NotFound),
        ];
        let ndjson = render(OutputFormat::Ndjson, false, &outcomes);
        let lines: Vec<&str> = ndjson.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            let _: serde_json::Value = serde_json::from_str(line).unwrap();
        }
    }

    #[test]
    fn html_output_is_a_document() {
        let outcomes = vec![outcome("GitHub", MatchKind::Found)];
        let html = render(OutputFormat::Html, false, &outcomes);
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.trim_end().ends_with("</html>"));
    }

    #[test]
    fn text_output_appends_correlation_when_present() {
        let outcomes = vec![outcome("GitHub", MatchKind::Found)];
        let report = CorrelationReport::default();
        let mut buf: Vec<u8> = Vec::new();
        write_outputs(
            &mut buf,
            &opts(OutputFormat::Text, false, false),
            &outcomes,
            Some(&report),
        )
        .unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("correlation:"), "{text}");
    }
}
