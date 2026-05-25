//! Adler CLI entry point.

mod report;
mod tui;

use std::io::{self, IsTerminal as _, Write};
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adler_core::browser::{BrowserbaseBackend, BrowserbaseConfig, LocalBackend, LocalConfig};
use adler_core::{
    BrowserBackend, Cache, CheckOutcome, Client, CorrelationReport, DoctorReport, ExecutorOptions,
    MatchKind, PermuteLevel, Registry, Site, Username, correlate, doctor, executor, permute,
};
use anyhow::{Context as _, Result};
use clap::{CommandFactory as _, Parser, ValueEnum};
use clap_complete::Shell;
use indicatif::{ProgressBar, ProgressStyle};
use tracing_subscriber::{EnvFilter, fmt};

const DEFAULT_CONCURRENCY: NonZeroUsize = match NonZeroUsize::new(32) {
    Some(n) => n,
    None => unreachable!(),
};

const AFTER_HELP: &str = concat!(
    "Examples:\n",
    "  adler alice\n",
    "  adler --only github,gitlab alice\n",
    "  adler --format json --no-progress alice\n",
    "  adler --exclude reddit --timeout 5 --concurrency 32 alice\n",
);

/// OSINT username search across many sites.
// CLI flag structs are naturally bool-heavy; the pedantic lint doesn't apply.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Parser)]
#[command(
    name = "adler",
    version,
    about,
    long_about = None,
    after_help = AFTER_HELP,
)]
struct Cli {
    /// Username to search for. With `--add-site`, this is an account that
    /// EXISTS on the site (used to derive the signature). Not required with
    /// `--doctor`, `--cache-clear`, `--list-sites`, or `--completions`.
    #[arg(required_unless_present_any = ["doctor", "cache_clear", "list_sites", "list_tags", "completions", "add_site", "input"])]
    username: Option<String>,

    /// Scan every username in this file (one per line; blank lines and lines
    /// starting with `#` are skipped, duplicates removed). A positional
    /// username, if given, is scanned too. Output is grouped per username;
    /// not compatible with `--tui` / `--correlate` / `--format html`.
    #[arg(long, value_name = "PATH")]
    input: Option<PathBuf>,

    /// List registry site names (honoring `--only`/`--exclude`/`--tag`) and
    /// exit. Handy for discovering filter terms among the bundled sites.
    #[arg(long)]
    list_sites: bool,

    /// List all tags in the registry with per-tag site counts, and exit.
    #[arg(long)]
    list_tags: bool,

    /// Only scan sites carrying one of these tags (e.g. `social`, `dev`,
    /// `region:ru`). Repeatable; comma-separated values also accepted.
    /// Sites with no tags are excluded when this is set.
    #[arg(long, value_delimiter = ',', value_name = "TAG")]
    tag: Vec<String>,

    /// Skip sites carrying any of these tags (e.g.
    /// `--exclude-tag bot-protected` for a fast clean run). Repeatable.
    #[arg(long, value_delimiter = ',', value_name = "TAG")]
    exclude_tag: Vec<String>,

    /// Print a shell completion script to stdout and exit.
    #[arg(long, value_enum, value_name = "SHELL")]
    completions: Option<Shell>,

    /// Run a signature health check on the registry instead of searching.
    /// For each site, probes the `known_present` user (if any) and a
    /// random nonsense user, then reports sites where verdicts violate
    /// expectations.
    #[arg(long)]
    doctor: bool,

    /// With `--doctor`: for each failing site, diff the present/absent
    /// responses and print a suggested signature (does not modify anything).
    #[arg(long, requires = "doctor")]
    fix: bool,

    /// With `--doctor`: for each failing site whose `known_present` is
    /// likely stale (no candidate yielded `Found`), probe a small pool
    /// of well-known accounts (`torvalds`, `octocat`, the site's brand
    /// name, …) and report the first one that resolves to `Found`.
    /// Prints a paste-ready `OVERRIDES` snippet for
    /// `scripts/import_sherlock.py`. Does not modify anything.
    #[arg(long, requires = "doctor")]
    suggest_known_present: bool,

    /// Scaffold a new site entry: probe this URL template (must contain
    /// `{username}`) with the given existing account and a nonsense one,
    /// derive a signature, and print a ready-to-paste JSON entry. Does not
    /// modify the registry. Combine with `--proxy` to probe from a clean IP.
    #[arg(long, value_name = "URL")]
    add_site: Option<String>,

    /// Site name for `--add-site` (defaults to the URL host).
    #[arg(long, value_name = "NAME", requires = "add_site")]
    name: Option<String>,

    /// Monitor mode: scan fresh, diff the found accounts against the last
    /// run's snapshot, report new/removed ones, and save a fresh snapshot
    /// (under the cache dir). Pair with `--interval` to keep watching.
    #[arg(long)]
    watch: bool,

    /// With `--watch`, re-scan every N seconds (continuous). One-shot if
    /// omitted (compose with cron yourself).
    #[arg(long, value_name = "SECS", requires = "watch")]
    interval: Option<u64>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,

    /// Override the embedded site list with a JSON file at this path.
    #[arg(long, value_name = "PATH")]
    sites: Option<PathBuf>,

    /// Only check sites whose name contains this substring (case-insensitive).
    /// Repeatable; comma-separated values also accepted.
    #[arg(long, value_delimiter = ',', value_name = "NAME")]
    only: Vec<String>,

    /// Exclude sites whose name contains this substring (case-insensitive).
    /// Repeatable; comma-separated values also accepted.
    #[arg(long, value_delimiter = ',', value_name = "NAME")]
    exclude: Vec<String>,

    /// Per-request timeout in seconds.
    #[arg(long, default_value_t = 10, value_name = "SECS")]
    timeout: u64,

    /// Max in-flight site checks.
    #[arg(long, default_value_t = DEFAULT_CONCURRENCY, value_name = "N")]
    concurrency: NonZeroUsize,

    /// Cap total requests/second across all hosts. Uncapped by default.
    #[arg(long, value_name = "RPS")]
    max_rps: Option<NonZeroU32>,

    /// Retry attempts after a transient ban (429 / Cloudflare). Default 2.
    /// Set 0 to disable — useful for `--doctor`, where a ban should surface
    /// immediately rather than being retried.
    #[arg(long, default_value_t = 2, value_name = "N")]
    max_retries: u32,

    /// Total scan deadline in seconds. Sites still in flight produce Uncertain outcomes.
    #[arg(long, value_name = "SECS")]
    deadline: Option<u64>,

    /// Show every site, including the (usually many) `NotFound` ones.
    /// By default the text output shows only Found and Uncertain results.
    #[arg(long)]
    all: bool,

    /// Under each result, print which signal(s) produced the verdict
    /// (e.g. `HTTP 404 (status_not_found)`). JSON always includes this.
    #[arg(long)]
    explain: bool,

    /// Print only found account URLs, one per line; suppress the progress
    /// bar, summary, and hints. Ideal for scripting.
    #[arg(short, long)]
    quiet: bool,

    /// When to colorize text output. `auto` (default) colors only an
    /// interactive terminal and honors the `NO_COLOR` environment variable.
    #[arg(long, value_enum, default_value_t = ColorChoice::Auto, value_name = "WHEN")]
    color: ColorChoice,

    /// Disable the progress bar even on an interactive terminal.
    #[arg(long)]
    no_progress: bool,

    /// Route all requests through a proxy (http://, https://, or socks5://).
    #[arg(long, value_name = "URL", conflicts_with = "tor")]
    proxy: Option<String>,

    /// Route through a local Tor SOCKS proxy (`socks5://127.0.0.1:9050`).
    #[arg(long)]
    tor: bool,

    /// Rotate the User-Agent header per request from a built-in browser pool.
    #[arg(long)]
    rotate_ua: bool,

    /// Honor each site's robots.txt: skip probes to disallowed paths
    /// (reported Uncertain). Adds one cached robots.txt fetch per host.
    #[arg(long)]
    respect_robots: bool,

    /// Browser backend used for sites tagged `bot-protected` (Instagram,
    /// X/Twitter, `TikTok`, Facebook, Threads, Snapchat, Weibo). `local`
    /// needs Chrome installed; `browserbase` reads
    /// `ADLER_BROWSERBASE_API_KEY` / `ADLER_BROWSERBASE_PROJECT_ID` and
    /// charges per session-minute. Default `none` leaves those sites on
    /// raw HTTP (typically Uncertain).
    #[arg(long, value_enum, default_value_t = BrowserBackendChoice::None, value_name = "BACKEND")]
    browser_backend: BrowserBackendChoice,

    /// Per-scan cap on browser-routed probes. Once exceeded, remaining
    /// bot-protected sites return `Uncertain(browser_budget_exceeded)`.
    /// Guardrail against a misconfigured flag burning a whole quota.
    #[arg(long, default_value_t = adler_core::DEFAULT_BROWSER_BUDGET, value_name = "N")]
    browser_budget: usize,

    /// Disable the browser backend for this run, even if `--browser-backend`
    /// or its env vars are set. Convenient for one-off raw-HTTP scans.
    #[arg(long)]
    no_browser: bool,

    /// Extract profile fields (name, bio, avatar, …) from found accounts on
    /// sites that declare extractor rules. Implies a fresh scan (skips the
    /// cache) so enrichment data is current.
    #[arg(long)]
    enrich: bool,

    /// Also search spelling variants of the username (separator swaps, leet,
    /// digit suffixes). Multiplies requests by the number of variants.
    #[arg(long, value_enum, default_value_t = Permute::None, value_name = "LEVEL")]
    permute: Permute,

    /// Group found accounts that look like the same person (by name/bio
    /// similarity) and print the clusters. Implies `--enrich`.
    #[arg(long)]
    correlate: bool,

    /// Browse results in an interactive terminal UI after the scan.
    /// Requires an interactive terminal; ignores `--format`.
    #[arg(long, conflicts_with = "format")]
    tui: bool,

    /// Skip the result cache for this run (no read, no write).
    #[arg(long)]
    no_cache: bool,

    /// Cache time-to-live in seconds. Entries older than this are ignored.
    #[arg(long, default_value_t = 3600, value_name = "SECS")]
    cache_ttl: u64,

    /// Override the cache file location.
    #[arg(long, value_name = "PATH")]
    cache_path: Option<PathBuf>,

    /// Delete the cache file and exit.
    #[arg(long)]
    cache_clear: bool,

    /// Append an NDJSON record per result (ts, username, site, url, kind)
    /// to this file, for an accountable trail of what was queried.
    #[arg(long, value_name = "PATH")]
    audit_log: Option<PathBuf>,
}

/// Built-in User-Agent pool used by `--rotate-ua`. Realistic recent
/// desktop browser strings; rotated uniformly at random per request.
const USER_AGENT_POOL: &[&str] = &[
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15",
    "Mozilla/5.0 (X11; Linux x86_64; rv:125.0) Gecko/20100101 Firefox/125.0",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:125.0) Gecko/20100101 Firefox/125.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
];

const TOR_PROXY: &str = "socks5://127.0.0.1:9050";

/// CLI mirror of [`PermuteLevel`] so clap parses it without coupling the
/// core type to clap.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Permute {
    None,
    Basic,
    Aggressive,
}

impl From<Permute> for PermuteLevel {
    fn from(p: Permute) -> Self {
        match p {
            Permute::None => Self::None,
            Permute::Basic => Self::Basic,
            Permute::Aggressive => Self::Aggressive,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    /// Human-readable summary with a final tally.
    Text,
    /// Pretty-printed JSON array of all outcomes.
    Json,
    /// One compact JSON object per line (ideal for `jq`/pipelines).
    Ndjson,
    /// Comma-separated values with a header row (spreadsheet-friendly).
    Csv,
    /// Self-contained HTML report (write to a file: `--format html > out.html`).
    Html,
}

/// Browser backend selection for `bot-protected` sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BrowserBackendChoice {
    /// Don't use a browser backend (default). Bot-protected sites stay on
    /// the raw HTTP path and typically return `Uncertain`.
    None,
    /// Launch local headless Chrome via `chromiumoxide`. Requires Chrome
    /// or Chromium installed; honors `--proxy` (passed via
    /// `--proxy-server=...`).
    Local,
    /// Browserbase cloud session. Reads credentials from
    /// `ADLER_BROWSERBASE_API_KEY` and `ADLER_BROWSERBASE_PROJECT_ID`.
    /// Pay-per-session — see the project README.
    Browserbase,
}

/// When to colorize text output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ColorChoice {
    /// Color only an interactive terminal, and not when `NO_COLOR` is set.
    Auto,
    /// Always color, even when piped.
    Always,
    /// Never color.
    Never,
}

impl ColorChoice {
    /// Resolve to a concrete on/off decision for a stream.
    ///
    /// `auto` colors only when `stdout` is a TTY and the `NO_COLOR`
    /// environment variable is unset (per <https://no-color.org>).
    fn resolve(self, is_tty: bool) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => is_tty && std::env::var_os("NO_COLOR").is_none(),
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.tui);
    match run(cli).await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("adler: {err:#}");
            ExitCode::from(2)
        }
    }
}

/// Install the stderr tracing subscriber.
///
/// Skipped under `--tui`: the interactive UI owns the terminal, so log lines
/// written to stderr would scribble over the rendered frame (and aren't
/// visible anyway). Run without `--tui` to see logs.
fn init_tracing(tui: bool) {
    if tui {
        return;
    }
    let filter =
        EnvFilter::try_from_env("ADLER_LOG").unwrap_or_else(|_| EnvFilter::new("adler=info"));
    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(io::stderr)
        .init();
}

fn cache_path(cli: &Cli) -> PathBuf {
    cli.cache_path.clone().unwrap_or_else(Cache::default_path)
}

async fn run(cli: Cli) -> Result<ExitCode> {
    if let Some(shell) = cli.completions {
        let mut cmd = Cli::command();
        clap_complete::generate(shell, &mut cmd, "adler", &mut io::stdout());
        return Ok(ExitCode::SUCCESS);
    }

    if cli.cache_clear {
        let path = cache_path(&cli);
        Cache::clear(&path).with_context(|| format!("clearing cache at {}", path.display()))?;
        println!("cleared cache at {}", path.display());
        return Ok(ExitCode::SUCCESS);
    }

    // Scaffolding a new site needs neither the registry nor a filter.
    if let Some(url) = cli.add_site.clone() {
        let client = build_client(&cli).await?;
        return run_add_site(&cli, &client, &url).await;
    }

    let registry = match &cli.sites {
        Some(path) => Registry::load_from_path(path)
            .with_context(|| format!("loading sites from {}", path.display()))?,
        None => Registry::default_embedded().context("loading embedded registry")?,
    };

    if cli.list_tags {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        for (tag, count) in registry.tag_counts() {
            writeln!(out, "{tag}\t{count}")?;
        }
        return Ok(ExitCode::SUCCESS);
    }

    let sites = registry.filter(&cli.only, &cli.exclude, &cli.tag, &cli.exclude_tag);
    if sites.is_empty() {
        eprintln!("adler: no sites match the filter");
        return Ok(ExitCode::from(2));
    }

    if cli.list_sites {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        for site in &sites {
            writeln!(out, "{}", site.name)?;
        }
        return Ok(ExitCode::SUCCESS);
    }

    let client = build_client(&cli).await?;

    if cli.doctor {
        let color = cli.color.resolve(io::stdout().is_terminal());
        return run_doctor(&client, &sites, cli.fix, cli.suggest_known_present, color).await;
    }

    run_scan(&cli, &client, &sites).await
}

/// Drive a username scan (with permutation variants), then emit results.
///
/// Split out of `run` so the dispatcher stays small; the network/I/O parts
/// live here while the pure pieces it relies on (`write_outputs`,
/// `any_found`) are unit-tested directly.
async fn run_scan(cli: &Cli, client: &Client, sites: &[Site]) -> Result<ExitCode> {
    let mut options = ExecutorOptions::default().concurrency(cli.concurrency);
    if let Some(d) = cli.deadline {
        options = options.deadline(Duration::from_secs(d));
    }

    if let Some(input) = cli.input.clone() {
        return run_batch(cli, client, sites, &input, options).await;
    }

    let username_raw = cli
        .username
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("username is required"))?;
    let username = Username::new(username_raw.to_owned()).context("invalid username")?;

    if cli.watch {
        return run_watch(cli, client, sites, &username, username_raw, options).await;
    }

    if cli.tui {
        if !io::stdout().is_terminal() {
            anyhow::bail!("--tui requires an interactive terminal");
        }
        return run_tui_live(cli, client, sites, &username, options).await;
    }

    // Load the cache once for the whole run (all permutation variants share
    // it), not per variant. --enrich / --correlate want fresh data, so they
    // bypass it. Each variant is a distinct username key within the cache.
    let use_cache = !cli.no_cache && !cli.enrich && !cli.correlate;
    let mut cache =
        use_cache.then(|| Cache::load(cache_path(cli), Duration::from_secs(cli.cache_ttl)));

    let stdout_tty = io::stdout().is_terminal();
    let display = DisplayOpts {
        show_all: cli.all,
        quiet: cli.quiet,
        color: cli.color.resolve(stdout_tty),
        explain: cli.explain,
    };
    // Interactive text streams each row live as it resolves; everything else
    // (piped text, JSON/NDJSON/HTML) collects first and emits at the end.
    // (`--tui` returned earlier.)
    let live = matches!(cli.format, OutputFormat::Text) && stdout_tty;

    let started = Instant::now();
    let outcomes = scan_one(
        cli,
        client,
        sites,
        &username,
        options,
        cache.as_mut(),
        live.then_some(display),
    )
    .await;
    if let Some(cache) = &cache {
        if let Err(err) = cache.save() {
            tracing::warn!(error = %err, "failed to persist cache");
        }
    }
    let elapsed = started.elapsed();

    if let Some(path) = &cli.audit_log {
        write_audit_log(path, username_raw, &outcomes)
            .with_context(|| format!("writing audit log to {}", path.display()))?;
    }

    let code = if any_found(&outcomes) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    };

    if live {
        // Rows already streamed during the scan; print the footer.
        if !cli.quiet {
            let stdout = io::stdout();
            let mut out = stdout.lock();
            print_tally(&mut out, &outcomes, elapsed)?;
            if cli.correlate {
                print_correlation(&mut out, &correlate(&outcomes))?;
            }
            print_hint(&mut out, cli, display.color)?;
        }
    } else {
        let opts = OutputOpts {
            format: cli.format,
            display,
            username: username_raw,
            elapsed,
        };
        let correlation = cli.correlate.then(|| correlate(&outcomes));
        let stdout = io::stdout();
        let mut out = stdout.lock();
        write_outputs(&mut out, &opts, &outcomes, correlation.as_ref())?;
    }

    Ok(code)
}

/// `--tui`: scan and browse concurrently. The scan runs as a background task
/// streaming each outcome over a channel; the (blocking) TUI event loop drains
/// the channel and renders live. Quitting the TUI aborts the scan. The result
/// cache is bypassed here (interactive exploration wants fresh data).
async fn run_tui_live(
    cli: &Cli,
    client: &Client,
    sites: &[Site],
    username: &Username,
    options: ExecutorOptions,
) -> Result<ExitCode> {
    let variants = permute(username, cli.permute.into());
    let (tx, rx) = std::sync::mpsc::channel::<CheckOutcome>();
    let client = client.clone();
    let sites = sites.to_vec();

    let scan = tokio::spawn(async move {
        for variant in &variants {
            let tx = tx.clone();
            executor::run_with_progress(&client, &sites, variant, options.clone(), move |o| {
                let _ = tx.send(o.clone());
            })
            .await;
        }
        // Dropping the last sender disconnects rx → clears "scanning…".
    });

    let tui_result = tokio::task::spawn_blocking(move || tui::run_live(&rx)).await;
    scan.abort(); // stop probing if the user quit before the scan finished
    tui_result
        .context("TUI task panicked")?
        .context("running TUI")?;
    Ok(ExitCode::SUCCESS)
}

/// New / removed found accounts between two scans of the same username.
struct WatchDiff {
    /// Accounts found now that weren't in the previous snapshot.
    added: Vec<CheckOutcome>,
    /// Site names that were found before but aren't now.
    removed: Vec<String>,
}

/// Diff the found-account set: which sites newly appeared, which disappeared.
/// Pure over its inputs — the testable core of `--watch`.
fn diff_found(previous: &[CheckOutcome], current: &[CheckOutcome]) -> WatchDiff {
    let prev_sites: std::collections::HashSet<&str> =
        previous.iter().map(|o| o.site.as_str()).collect();
    let now_sites: std::collections::HashSet<&str> = current
        .iter()
        .filter(|o| o.kind.is_found())
        .map(|o| o.site.as_str())
        .collect();

    let added = current
        .iter()
        .filter(|o| o.kind.is_found() && !prev_sites.contains(o.site.as_str()))
        .cloned()
        .collect();
    let removed = previous
        .iter()
        .filter(|o| !now_sites.contains(o.site.as_str()))
        .map(|o| o.site.clone())
        .collect();
    WatchDiff { added, removed }
}

/// Snapshot file for a username: `<cache dir>/watch/<username>.json`.
fn watch_snapshot_path(cli: &Cli, username: &str) -> PathBuf {
    cache_path(cli)
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default()
        .join("watch")
        .join(format!("{username}.json"))
}

/// Load the previous found-account snapshot, or `None` if there isn't one.
fn load_watch_snapshot(path: &std::path::Path) -> Option<Vec<CheckOutcome>> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// `--watch`: scan, diff against the last snapshot, report, save. Loops on
/// `--interval`, otherwise one-shot.
async fn run_watch(
    cli: &Cli,
    client: &Client,
    sites: &[Site],
    username: &Username,
    username_raw: &str,
    options: ExecutorOptions,
) -> Result<ExitCode> {
    if cli.tui {
        anyhow::bail!("--watch is not compatible with --tui");
    }
    let path = watch_snapshot_path(cli, username_raw);

    if let Some(secs) = cli.interval {
        let period = Duration::from_secs(secs.max(1));
        loop {
            watch_cycle(
                cli,
                client,
                sites,
                username,
                username_raw,
                &path,
                options.clone(),
            )
            .await?;
            tokio::time::sleep(period).await;
        }
    } else {
        watch_cycle(cli, client, sites, username, username_raw, &path, options).await?;
        Ok(ExitCode::SUCCESS)
    }
}

/// One scan-and-diff cycle for `--watch`.
async fn watch_cycle(
    cli: &Cli,
    client: &Client,
    sites: &[Site],
    username: &Username,
    username_raw: &str,
    path: &std::path::Path,
    options: ExecutorOptions,
) -> Result<()> {
    let outcomes = scan_one(cli, client, sites, username, options, None, None).await;
    let found_now: Vec<&CheckOutcome> = outcomes.iter().filter(|o| o.kind.is_found()).collect();

    let stdout = io::stdout();
    let mut out = stdout.lock();
    match load_watch_snapshot(path) {
        None => {
            writeln!(
                out,
                "watch {username_raw}: baseline recorded — {} found",
                found_now.len()
            )?;
        }
        Some(previous) => {
            let diff = diff_found(&previous, &outcomes);
            if diff.added.is_empty() && diff.removed.is_empty() {
                writeln!(
                    out,
                    "watch {username_raw}: no change — {} found",
                    found_now.len()
                )?;
            } else {
                writeln!(out, "watch {username_raw}: {} found", found_now.len())?;
                for o in &diff.added {
                    writeln!(out, "  + new:  {:<16} {}", o.site, o.url)?;
                }
                for site in &diff.removed {
                    writeln!(out, "  - gone: {site}")?;
                }
            }
        }
    }

    // Persist the current found set as the new baseline.
    std::fs::create_dir_all(path.parent().unwrap_or(path))
        .with_context(|| format!("creating watch dir for {}", path.display()))?;
    let json = serde_json::to_string(&found_now).context("serializing snapshot")?;
    std::fs::write(path, json).with_context(|| format!("writing snapshot {}", path.display()))?;
    Ok(())
}

/// Scan one username across `sites` (expanding permutation variants),
/// returning all outcomes. Shared by the single-username and batch paths.
async fn scan_one(
    cli: &Cli,
    client: &Client,
    sites: &[Site],
    username: &Username,
    options: ExecutorOptions,
    mut cache: Option<&mut Cache>,
    stream: Option<DisplayOpts>,
) -> Vec<CheckOutcome> {
    let variants = permute(username, cli.permute.into());
    tracing::info!(sites = sites.len(), variants = variants.len(), user = %username, "scanning");
    let mut outcomes = Vec::new();
    for variant in &variants {
        outcomes.extend(
            scan(
                cli,
                client,
                sites,
                variant,
                options.clone(),
                cache.as_deref_mut(),
                stream,
            )
            .await,
        );
    }
    outcomes
}

/// Read usernames for `--input`: one per line, `#` comments and blanks
/// skipped, duplicates removed (order preserved). A positional username, if
/// present, is scanned first.
fn read_usernames(path: &std::path::Path, positional: Option<&str>) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading usernames from {}", path.display()))?;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let positional = positional.into_iter().map(str::to_owned);
    for raw in positional.chain(text.lines().map(str::to_owned)) {
        let u = raw.trim();
        if u.is_empty() || u.starts_with('#') {
            continue;
        }
        if seen.insert(u.to_owned()) {
            out.push(u.to_owned());
        }
    }
    Ok(out)
}

/// `--input`: scan a list of usernames through a shared cache, grouped output.
async fn run_batch(
    cli: &Cli,
    client: &Client,
    sites: &[Site],
    input: &std::path::Path,
    options: ExecutorOptions,
) -> Result<ExitCode> {
    if cli.tui {
        anyhow::bail!("--input is not compatible with --tui");
    }
    if cli.correlate {
        anyhow::bail!("--input is not compatible with --correlate");
    }
    if matches!(cli.format, OutputFormat::Html) {
        anyhow::bail!("--input does not support --format html (use text/json/ndjson)");
    }

    let usernames = read_usernames(input, cli.username.as_deref())?;
    if usernames.is_empty() {
        anyhow::bail!("no usernames found in {}", input.display());
    }
    tracing::info!(
        count = usernames.len(),
        sites = sites.len(),
        "starting batch scan"
    );

    let use_cache = !cli.no_cache && !cli.enrich && !cli.correlate;
    let mut cache =
        use_cache.then(|| Cache::load(cache_path(cli), Duration::from_secs(cli.cache_ttl)));

    let started = Instant::now();
    let mut results: Vec<(String, Vec<CheckOutcome>)> = Vec::new();
    for raw in &usernames {
        let username = match Username::new(raw.clone()) {
            Ok(u) => u,
            Err(err) => {
                eprintln!("adler: skipping {raw:?}: {err}");
                continue;
            }
        };
        let outcomes = scan_one(
            cli,
            client,
            sites,
            &username,
            options.clone(),
            cache.as_mut(),
            None,
        )
        .await;
        if let Some(path) = &cli.audit_log {
            write_audit_log(path, raw, &outcomes)
                .with_context(|| format!("writing audit log to {}", path.display()))?;
        }
        results.push((raw.clone(), outcomes));
    }
    if let Some(cache) = &cache {
        if let Err(err) = cache.save() {
            tracing::warn!(error = %err, "failed to persist cache");
        }
    }
    let elapsed = started.elapsed();

    let display = DisplayOpts {
        show_all: cli.all,
        quiet: cli.quiet,
        color: cli.color.resolve(io::stdout().is_terminal()),
        explain: cli.explain,
    };
    let stdout = io::stdout();
    let mut out = stdout.lock();
    write_batch(&mut out, cli.format, display, &results, elapsed)?;

    let any = results.iter().any(|(_, outcomes)| any_found(outcomes));
    Ok(if any {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Emit batch results grouped per username (text / JSON / NDJSON).
fn write_batch(
    out: &mut impl Write,
    format: OutputFormat,
    display: DisplayOpts,
    results: &[(String, Vec<CheckOutcome>)],
    elapsed: Duration,
) -> Result<()> {
    match format {
        OutputFormat::Text => {
            for (username, outcomes) in results {
                let mut sorted: Vec<&CheckOutcome> = outcomes.iter().collect();
                sorted.sort_by(|a, b| a.site.cmp(&b.site));
                if display.quiet {
                    for o in sorted.iter().filter(|o| o.kind.is_found()) {
                        writeln!(out, "{username}\t{}", o.url)?;
                    }
                    continue;
                }
                let found = outcomes.iter().filter(|o| o.kind.is_found()).count();
                writeln!(out, "== {username} ({found} found) ==")?;
                for o in &sorted {
                    if should_show(o.kind, display.show_all) {
                        print_row(out, o, display)?;
                    }
                }
                writeln!(out)?;
            }
            if !display.quiet {
                let total: usize = results
                    .iter()
                    .map(|(_, o)| o.iter().filter(|x| x.kind.is_found()).count())
                    .sum();
                writeln!(
                    out,
                    "{} usernames · {total} found total · {:.2}s",
                    results.len(),
                    elapsed.as_secs_f64()
                )?;
            }
            Ok(())
        }
        OutputFormat::Json => {
            let arr: Vec<_> = results
                .iter()
                .map(|(u, o)| serde_json::json!({ "username": u, "results": o }))
                .collect();
            serde_json::to_writer_pretty(&mut *out, &arr).context("writing JSON")?;
            writeln!(out).context("writing JSON newline")
        }
        OutputFormat::Ndjson => {
            for (username, outcomes) in results {
                for outcome in outcomes {
                    let mut value = serde_json::to_value(outcome).context("encoding outcome")?;
                    if let Some(map) = value.as_object_mut() {
                        map.insert("username".to_owned(), serde_json::json!(username));
                    }
                    serde_json::to_writer(&mut *out, &value).context("writing NDJSON")?;
                    writeln!(out).context("writing NDJSON newline")?;
                }
            }
            Ok(())
        }
        OutputFormat::Csv => {
            writeln!(out, "username,{CSV_COLUMNS}").context("writing CSV header")?;
            for (username, outcomes) in results {
                let mut sorted: Vec<&CheckOutcome> = outcomes.iter().collect();
                sorted.sort_by(|a, b| a.site.cmp(&b.site));
                for o in &sorted {
                    let mut fields = vec![username.clone()];
                    fields.extend(outcome_csv_fields(o));
                    write_csv_row(out, &fields).context("writing CSV row")?;
                }
            }
            Ok(())
        }
        OutputFormat::Html => anyhow::bail!("--format html is not supported in batch mode"),
    }
}

/// Append one NDJSON audit record per outcome to `path` (append/create).
///
/// All records share the scan's completion timestamp — we track per-probe
/// elapsed time, not absolute request time, so a single scan timestamp is
/// the honest granularity here.
fn write_audit_log(
    path: &std::path::Path,
    username: &str,
    outcomes: &[CheckOutcome],
) -> Result<()> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    for o in outcomes {
        let record = serde_json::json!({
            "ts": ts,
            "username": username,
            "site": o.site,
            "url": o.url,
            "kind": o.kind,
        });
        writeln!(file, "{record}")?;
    }
    Ok(())
}

/// Write the cross-account correlation summary (text format).
fn print_correlation(out: &mut impl Write, report: &CorrelationReport) -> io::Result<()> {
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

/// Scan `sites` for one `username`, consulting and updating the shared
/// `cache` if one is provided.
///
/// Cache-hit sites are resolved without network I/O; the rest go through the
/// executor and their fresh `Found` / `NotFound` verdicts are written back.
/// The caller owns the cache's lifecycle (load once, save once) so a
/// multi-variant run doesn't re-read/re-write the file per variant.
async fn scan(
    cli: &Cli,
    client: &Client,
    sites: &[Site],
    username: &Username,
    options: ExecutorOptions,
    cache: Option<&mut Cache>,
    stream: Option<DisplayOpts>,
) -> Vec<CheckOutcome> {
    let mut cached: Vec<CheckOutcome> = Vec::new();
    let mut to_probe: Vec<Site> = Vec::new();
    if let Some(c) = cache.as_deref() {
        for site in sites {
            match c.get(site, username) {
                Some(outcome) => cached.push(outcome),
                None => to_probe.push(site.clone()),
            }
        }
        tracing::info!(
            hits = cached.len(),
            misses = to_probe.len(),
            "cache consulted"
        );
    } else {
        to_probe = sites.to_vec();
    }

    let show_progress = !cli.no_progress
        && !cli.quiet
        && io::stderr().is_terminal()
        && matches!(cli.format, OutputFormat::Text);
    let progress = show_progress.then(|| {
        let bar = make_progress_bar(u64::try_from(sites.len()).unwrap_or(u64::MAX));
        bar.inc(u64::try_from(cached.len()).unwrap_or(0)); // count cache hits
        bar
    });

    // Stream cache hits immediately (they resolved with no network wait).
    if let Some(disp) = stream {
        for o in &cached {
            match &progress {
                Some(bar) => bar.suspend(|| stream_row(o, disp)),
                None => stream_row(o, disp),
            }
        }
    }

    let fresh = if let Some(bar) = &progress {
        let probe_bar = bar.clone();
        executor::run_with_progress(client, &to_probe, username, options, move |o| {
            probe_bar.inc(1);
            if let Some(disp) = stream {
                probe_bar.suspend(|| stream_row(o, disp));
            }
        })
        .await
    } else if let Some(disp) = stream {
        executor::run_with_progress(client, &to_probe, username, options, move |o| {
            stream_row(o, disp);
        })
        .await
    } else {
        executor::run(client, &to_probe, username, options).await
    };
    if let Some(bar) = progress {
        bar.finish_and_clear();
    }

    if let Some(c) = cache {
        let by_name: std::collections::HashMap<&str, &Site> =
            to_probe.iter().map(|s| (s.name.as_str(), s)).collect();
        for outcome in &fresh {
            if let Some(site) = by_name.get(outcome.site.as_str()) {
                c.put(site, username, outcome.clone());
            }
        }
    }

    cached.into_iter().chain(fresh).collect()
}

async fn build_client(cli: &Cli) -> Result<Client> {
    let mut builder = Client::builder()
        .timeout(Duration::from_secs(cli.timeout))
        .max_retries(cli.max_retries);
    if let Some(rps) = cli.max_rps {
        builder = builder.max_rps(rps);
    }
    let proxy_for_browser: Option<String> = if cli.tor {
        builder = builder.proxy(TOR_PROXY);
        Some(TOR_PROXY.to_owned())
    } else if let Some(url) = &cli.proxy {
        builder = builder.proxy(url.clone());
        Some(url.clone())
    } else {
        None
    };
    if cli.rotate_ua {
        builder =
            builder.rotate_user_agents(USER_AGENT_POOL.iter().map(|s| (*s).to_owned()).collect());
    }

    if let Some(backend) = build_browser_backend(cli, proxy_for_browser.as_deref()).await? {
        builder = builder.browser(backend).browser_budget(cli.browser_budget);
    }

    builder
        // --correlate needs profile fields, so it implies enrichment.
        .enrich(cli.enrich || cli.correlate)
        .respect_robots(cli.respect_robots)
        .build()
        .context("building HTTP client")
}

/// Construct the browser backend selected by CLI flags, or `None` when no
/// backend should be used. `--no-browser` short-circuits to `None` even if
/// a backend is configured.
async fn build_browser_backend(
    cli: &Cli,
    proxy_url: Option<&str>,
) -> Result<Option<Arc<dyn BrowserBackend>>> {
    if cli.no_browser {
        return Ok(None);
    }
    match cli.browser_backend {
        BrowserBackendChoice::None => Ok(None),
        BrowserBackendChoice::Local => {
            let cfg = LocalConfig {
                proxy_url: proxy_url.map(str::to_owned),
            };
            let backend = LocalBackend::launch(cfg)
                .await
                .context("launching local browser backend (is Chrome installed?)")?;
            eprintln!(
                "adler: launched local Chrome for bot-protected sites (budget: {})",
                cli.browser_budget
            );
            Ok(Some(Arc::new(backend) as Arc<dyn BrowserBackend>))
        }
        BrowserBackendChoice::Browserbase => {
            let api_key = std::env::var("ADLER_BROWSERBASE_API_KEY").map_err(|_| {
                anyhow::anyhow!(
                    "--browser-backend browserbase requires ADLER_BROWSERBASE_API_KEY env var"
                )
            })?;
            let project_id = std::env::var("ADLER_BROWSERBASE_PROJECT_ID").map_err(|_| {
                anyhow::anyhow!(
                    "--browser-backend browserbase requires ADLER_BROWSERBASE_PROJECT_ID env var"
                )
            })?;
            let cfg = BrowserbaseConfig {
                api_key: secrecy::SecretString::from(api_key),
                project_id,
            };
            let backend = BrowserbaseBackend::connect(cfg)
                .await
                .context("opening Browserbase session")?;
            // Cost reality check, on stderr so it survives stdout redirects.
            // Stays terse so it doesn't drown the progress bar.
            eprintln!(
                "adler: opened Browserbase session (id={}) — sites tagged bot-protected will route through it, billed per session-minute. Budget: {}.",
                backend.session_id(),
                cli.browser_budget,
            );
            Ok(Some(Arc::new(backend) as Arc<dyn BrowserBackend>))
        }
    }
}

/// Derive a default site name from a URL: the registrable label of the host,
/// title-cased (e.g. `https://www.example.com/{username}` → `Example`).
fn derive_name(url: &str) -> String {
    let host = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or("")
        .trim_start_matches("www.");
    let label = host.split('.').next().unwrap_or(host);
    let mut chars = label.chars();
    chars.next().map_or_else(
        || "Site".to_owned(),
        |first| first.to_uppercase().collect::<String>() + chars.as_str(),
    )
}

/// `--add-site`: probe a URL with a known account + a nonsense one, derive a
/// signature, and print a ready-to-paste site entry.
async fn run_add_site(cli: &Cli, client: &Client, url: &str) -> Result<ExitCode> {
    let known = cli.username.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "--add-site needs a username that exists on the site \
             (it's used to derive the signature): adler --add-site \"{url}\" <existing-user>"
        )
    })?;
    let name = cli.name.clone().unwrap_or_else(|| derive_name(url));

    let scaffold = doctor::scaffold_site(client, &name, url, known)
        .await
        .context("probing site for --add-site")?;

    if let Some((site, rationale)) = scaffold {
        eprintln!("derived signature ({rationale})");
        eprintln!("add this to scripts/import_sherlock.py OVERRIDES (or sites.json):\n");
        let json = serde_json::to_string_pretty(&site).context("serializing site entry")?;
        println!("{json}");
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!(
            "adler: couldn't derive a signature — the responses for {known:?} and a \
             nonsense user look identical.\nLikely causes: {known:?} doesn't actually \
             exist there, or the site is bot-protected (serves the same page to \
             everyone). Try a stable API/feed endpoint, or re-run through --proxy with \
             a clean IP."
        );
        Ok(ExitCode::from(1))
    }
}

async fn run_doctor(
    client: &Client,
    sites: &[Site],
    fix: bool,
    suggest_known_present: bool,
    color: bool,
) -> Result<ExitCode> {
    tracing::info!(
        count = sites.len(),
        fix,
        suggest_known_present,
        "starting doctor"
    );
    let mut failures = 0_usize;
    let mut failed_sites: Vec<&Site> = Vec::new();
    for site in sites {
        let report = doctor::check_site(client, site).await;
        match report {
            DoctorReport::Healthy { .. } => {
                if color {
                    println!("\x1b[32m[OK]\x1b[0m   {}", site.name);
                } else {
                    println!("[OK]   {}", site.name);
                }
            }
            DoctorReport::Unhealthy { issues, .. } => {
                failures += 1;
                failed_sites.push(site);
                if color {
                    println!("\x1b[31m[FAIL]\x1b[0m {}", site.name);
                } else {
                    println!("[FAIL] {}", site.name);
                }
                for issue in &issues {
                    println!("       · {issue}");
                }
            }
        }
    }
    println!();
    println!("{} site(s) checked, {failures} failed", sites.len());

    if fix && !failed_sites.is_empty() {
        print_fix_suggestions(client, &failed_sites).await?;
    }

    if suggest_known_present && !failed_sites.is_empty() {
        print_known_present_suggestions(client, &failed_sites).await?;
    }

    Ok(if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Diff present/absent responses for each failing site and print a suggested
/// signature snippet. Suggestions are advisory — nothing is modified.
async fn print_fix_suggestions(client: &Client, failed: &[&Site]) -> Result<()> {
    println!("\nsuggested fixes (review before applying — paste into a --sites file):\n");
    let mut suggested = 0_usize;
    for site in failed {
        match doctor::suggest_fix(client, site).await {
            Some(fix) => {
                suggested += 1;
                let signals =
                    serde_json::to_string(&fix.signals).unwrap_or_else(|_| "[]".to_owned());
                println!("  {}  ({})", fix.site, fix.rationale);
                println!(
                    "    {{\"name\": {:?}, \"url\": {:?}, \"signals\": {}}}",
                    site.name,
                    site.url.as_str(),
                    signals,
                );
            }
            None => {
                println!(
                    "  {}  — no suggestion (responses indistinguishable; likely a stale known_present)",
                    site.name
                );
            }
        }
    }
    println!(
        "\n{suggested} of {} failing site(s) produced a suggestion",
        failed.len()
    );
    Ok(())
}

/// For each failing site, probe a small pool of well-known accounts and
/// report the first one (if any) that resolves to `Found`. Output is a
/// paste-ready snippet for `scripts/import_sherlock.py:OVERRIDES`.
/// Nothing is modified — the maintainer reviews and pastes.
async fn print_known_present_suggestions(client: &Client, failed: &[&Site]) -> Result<()> {
    println!("\nknown_present discovery (paste into scripts/import_sherlock.py OVERRIDES):\n");
    let mut found_count = 0_usize;
    let mut snippets: Vec<String> = Vec::new();
    for site in failed {
        let pool = doctor::default_candidate_pool(site);
        match doctor::discover_known_present(client, site, &pool).await {
            Some(name) => {
                found_count += 1;
                println!("  {}  ← {name:?}", site.name);
                snippets.push(format!(
                    "    {:?}: {{\"known_present\": {name:?}}},",
                    site.name,
                ));
            }
            None => {
                println!(
                    "  {}  — no candidate matched (tried {} usernames)",
                    site.name,
                    pool.len()
                );
            }
        }
    }
    if !snippets.is_empty() {
        println!("\nOVERRIDES additions:");
        for line in &snippets {
            println!("{line}");
        }
    }
    println!(
        "\n{found_count} of {} failing site(s) yielded a known_present candidate",
        failed.len()
    );
    Ok(())
}

fn make_progress_bar(total: u64) -> ProgressBar {
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
fn any_found(outcomes: &[CheckOutcome]) -> bool {
    outcomes.iter().any(|o| o.kind.is_found())
}

/// What and how to print each text result row.
// Display toggles are naturally bool-heavy; the pedantic lint doesn't apply.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy)]
struct DisplayOpts {
    /// Show `NotFound` rows too (default hides the bulk noise).
    show_all: bool,
    /// Print only found URLs, no chrome (`--quiet`).
    quiet: bool,
    /// Colorize rows.
    color: bool,
    /// Print the signal evidence under each row (`--explain`).
    explain: bool,
}

/// Presentation options for [`write_outputs`].
struct OutputOpts<'a> {
    format: OutputFormat,
    display: DisplayOpts,
    username: &'a str,
    elapsed: Duration,
}

/// Whether a verdict should appear in human output. `Found` and `Uncertain`
/// are always shown; `NotFound` is the bulk noise, hidden unless `show_all`.
fn should_show(kind: MatchKind, show_all: bool) -> bool {
    show_all || kind != MatchKind::NotFound
}

/// Print one result row. In quiet mode only `Found` rows print, as a bare URL.
fn print_row(out: &mut impl Write, o: &CheckOutcome, disp: DisplayOpts) -> io::Result<()> {
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
    if disp.explain {
        for line in &o.evidence {
            writeln!(out, "    why: {line}")?;
        }
    }
    for (field, value) in &o.enrichment {
        writeln!(out, "    {field}: {value}")?;
    }
    Ok(())
}

/// Print the final tally line, counted over *all* outcomes regardless of
/// what was displayed.
fn print_tally(
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
fn print_hint(out: &mut impl Write, cli: &Cli, color: bool) -> io::Result<()> {
    let mut tips: Vec<&str> = Vec::new();
    if !cli.enrich && !cli.correlate {
        tips.push("--enrich for profiles");
    }
    tips.push("--tui to browse");
    tips.push("--format json to script");
    let line = format!("tip: {}", tips.join(" · "));
    if color {
        writeln!(out, "\x1b[2m{line}\x1b[0m")
    } else {
        writeln!(out, "{line}")
    }
}

/// Print one result row to stdout (used by the live streaming callback).
fn stream_row(o: &CheckOutcome, disp: DisplayOpts) {
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
fn csv_escape(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_owned()
    }
}

/// Write one CSV record (escaped, comma-joined, CRLF-free fields).
fn write_csv_row(out: &mut impl Write, fields: &[String]) -> io::Result<()> {
    let escaped: Vec<String> = fields.iter().map(|f| csv_escape(f)).collect();
    writeln!(out, "{}", escaped.join(","))
}

/// The per-outcome CSV columns (after any leading `username` in batch mode).
fn outcome_csv_fields(o: &CheckOutcome) -> Vec<String> {
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

const CSV_COLUMNS: &str = "site,url,kind,reason,elapsed_ms,evidence";

/// Render outcomes (and optional correlation) to `out` in the chosen format.
///
/// Pure in its inputs and the writer — no stdout locking or terminal probing
/// here, so it's unit-testable against an in-memory buffer. This is the batch
/// path (piped text, JSON, NDJSON, CSV, HTML); interactive text streams rows
/// live during the scan instead (see `run_scan`).
fn write_outputs(
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
    fn diff_found_reports_added_and_removed() {
        let prev = vec![
            outcome("GitHub", MatchKind::Found),
            outcome("Reddit", MatchKind::Found),
        ];
        let now = vec![
            outcome("GitHub", MatchKind::Found),    // unchanged
            outcome("Reddit", MatchKind::NotFound), // disappeared
            outcome("GitLab", MatchKind::Found),    // new
            outcome("Vimeo", MatchKind::Uncertain), // not a found → ignored
        ];
        let diff = diff_found(&prev, &now);
        let added: Vec<&str> = diff.added.iter().map(|o| o.site.as_str()).collect();
        assert_eq!(added, ["GitLab"], "only newly-found sites");
        assert_eq!(diff.removed, ["Reddit"], "sites no longer found");
    }

    #[test]
    fn diff_found_empty_when_unchanged() {
        let prev = vec![outcome("GitHub", MatchKind::Found)];
        let now = vec![
            outcome("GitHub", MatchKind::Found),
            outcome("X", MatchKind::NotFound),
        ];
        let diff = diff_found(&prev, &now);
        assert!(diff.added.is_empty() && diff.removed.is_empty());
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
    fn derive_name_titlecases_host_label() {
        assert_eq!(derive_name("https://www.example.com/{username}"), "Example");
        assert_eq!(derive_name("https://github.com/{username}"), "Github");
        assert_eq!(derive_name("http://sub.example.co.uk/u/{username}"), "Sub");
        assert_eq!(derive_name("not a url"), "Not a url");
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
    fn color_choice_resolves_against_tty_and_no_color() {
        assert!(ColorChoice::Always.resolve(false));
        assert!(!ColorChoice::Never.resolve(true));
        // Auto depends on TTY; NO_COLOR handling is covered by the env check
        // in `resolve` itself (not exercised here to avoid mutating env).
        assert!(!ColorChoice::Auto.resolve(false));
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
