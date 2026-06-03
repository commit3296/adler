//! Adler CLI entry point.

mod report;

use std::io::{self, IsTerminal as _, Write};
use std::net::SocketAddr;
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adler_core::browser::{BrowserbaseBackend, BrowserbaseConfig, LocalBackend, LocalConfig};
use adler_core::{
    BrowserBackend, Cache, CheckOutcome, Client, CorrelationReport, DoctorReport, EgressSpec,
    ExecutorOptions, MatchKind, PermuteLevel, Registry, Session, SessionStore, Site, Username,
    correlate, doctor, executor, permute,
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
    "  Basics:\n",
    "    adler alice\n",
    "    adler --only github,gitlab alice           # restrict to matching names\n",
    "    adler --tag dev,social alice                # filter by tags\n",
    "    adler --top 50 alice                        # popular sites only\n",
    "\n",
    "  Output for tools and pipelines:\n",
    "    adler --format ndjson alice | jq -r 'select(.kind==\"found\") | .url'\n",
    "    adler --format csv alice > alice.csv\n",
    "    adler --format html alice > alice.html      # self-contained report\n",
    "    adler --quiet alice                         # found URLs only, scripting\n",
    "    adler --explain alice                       # show signal evidence per row\n",
    "\n",
    "  Access engine (reach the hard sites):\n",
    "    adler --proxy socks5://user:pass@host:1080 alice\n",
    "    adler --proxy-pool egress.toml alice        # per-site geo routing\n",
    "    adler --browser-backend local alice         # bot-protected via Chrome\n",
    "    adler --sessions sessions.toml alice        # login-walled sites\n",
    "    adler --no-escalation alice                 # cheap-path verdicts only\n",
    "\n",
    "  Doctor (validate and heal the registry):\n",
    "    adler --doctor --only github                # spot-check one site\n",
    "    adler --doctor --fix --suggest-known-present\n",
    "    adler --doctor --fix --apply --sites overrides.json --yes\n",
    "    adler --doctor --suggest-protection         # telemetry-fed tagging\n",
    "\n",
    "  Batch and watch:\n",
    "    adler --input users.txt --format ndjson > batch.ndjson\n",
    "    adler --watch --interval 86400 alice        # daily diff vs last run\n",
    "\n",
    "  Web UI:\n",
    "    adler --web                                 # http://127.0.0.1:8765\n",
    "    adler --web --web-bind 0.0.0.0:8765         # LAN — trusted network only\n",
    "\n",
    "Full reference: https://adler-docs.pages.dev/\n",
);

/// Multi-line `--version` body. Carries enough provenance for a bug
/// report (build commit, target triple, opt-in feature flags) without
/// requiring the maintainer to ask "what version are you on?" three
/// times. Built in `build.rs`; falls back gracefully when build-time
/// git capture is empty (e.g. a `cargo install` from a crates.io
/// tarball outside a git checkout).
const LONG_VERSION: &str = include_str!(concat!(env!("OUT_DIR"), "/long_version.txt"));

/// OSINT username search across many sites.
// CLI flag structs are naturally bool-heavy; the pedantic lint doesn't apply.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Parser)]
#[command(
    name = "adler",
    version,
    long_version = LONG_VERSION,
    about,
    long_about = None,
    after_help = AFTER_HELP,
)]
struct Cli {
    /// Username to search for. With `--add-site`, this is an account that
    /// EXISTS on the site (used to derive the signature). Not required with
    /// `--doctor`, `--cache-clear`, `--list-sites`, or `--completions`.
    #[arg(required_unless_present_any = ["doctor", "cache_clear", "list_sites", "list_tags", "completions", "man_page", "add_site", "input", "web"])]
    username: Option<String>,

    /// Scan every username in this file (one per line; blank lines and lines
    /// starting with `#` are skipped, duplicates removed). A positional
    /// username, if given, is scanned too. Output is grouped per username;
    /// not compatible with `--correlate` / `--format html`.
    #[arg(long, value_name = "PATH", help_heading = "Batch & enrichment")]
    input: Option<PathBuf>,

    /// List registry site names (honoring `--only`/`--exclude`/`--tag`) and
    /// exit. Handy for discovering filter terms among the bundled sites.
    #[arg(long, help_heading = "Filtering")]
    list_sites: bool,

    /// List all tags in the registry with per-tag site counts, and exit.
    #[arg(long, help_heading = "Filtering")]
    list_tags: bool,

    /// Only scan sites carrying one of these tags (e.g. `social`, `dev`,
    /// `region:ru`). Repeatable; comma-separated values also accepted.
    /// Sites with no tags are excluded when this is set.
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "TAG",
        help_heading = "Filtering"
    )]
    tag: Vec<String>,

    /// Skip sites carrying any of these tags (e.g.
    /// `--exclude-tag bot-protected` for a fast clean run). Repeatable.
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "TAG",
        help_heading = "Filtering"
    )]
    exclude_tag: Vec<String>,

    /// Include sites tagged `nsfw` (adult content) in the scan. They are
    /// auto-excluded by default — surfacing a profile URL on Pornhub
    /// or Xvideos when the user just typed `adler alice` is rarely what
    /// they wanted. Passing `--tag nsfw` also opts in.
    #[arg(long, help_heading = "Filtering")]
    nsfw: bool,

    /// Print a shell completion script to stdout and exit.
    #[arg(long, value_enum, value_name = "SHELL", help_heading = "Misc")]
    completions: Option<Shell>,

    /// Print a roff(1) man page to stdout and exit. Intended for distro
    /// packagers: `adler --man-page > /usr/share/man/man1/adler.1`.
    #[arg(long, help_heading = "Misc")]
    man_page: bool,

    /// Run a signature health check on the registry instead of searching.
    /// For each site, probes the `known_present` user (if any) and a
    /// random nonsense user, then reports sites where verdicts violate
    /// expectations.
    #[arg(long, help_heading = "Doctor")]
    doctor: bool,

    /// With `--doctor`: for each failing site, diff the present/absent
    /// responses and print a suggested signature (does not modify anything).
    #[arg(long, requires = "doctor", help_heading = "Doctor")]
    fix: bool,

    /// With `--doctor --fix`: patch the file passed via `--sites` in
    /// place with the suggested signals (atomic write). The embedded
    /// registry is read-only — pass `--sites <path>` to a writable
    /// JSON file. By default, prompts once after printing the diff;
    /// pass `--yes` to skip the prompt for non-interactive use.
    #[arg(long, requires_all = ["fix", "sites"], help_heading = "Doctor")]
    apply: bool,

    /// With `--apply`: skip the confirmation prompt. Intended for
    /// scripted use (CI doctor jobs, batch repair); interactive runs
    /// should leave this off and review the diff.
    #[arg(long, requires = "apply", help_heading = "Doctor")]
    yes: bool,

    /// With `--doctor`: for each failing site whose `known_present` is
    /// likely stale (no candidate yielded `Found`), probe a small pool
    /// of well-known accounts (`torvalds`, `octocat`, the site's brand
    /// name, …) and report the first one that resolves to `Found`.
    /// Prints a paste-ready `OVERRIDES` snippet for
    /// `scripts/import_sherlock.py`. Does not modify anything.
    #[arg(long, requires = "doctor", help_heading = "Doctor")]
    suggest_known_present: bool,

    /// With `--doctor`: read the persisted scan history (default
    /// `$XDG_CACHE_HOME/adler/scans/`, override with `--scans-dir`)
    /// and surface sites that consistently escalated through the
    /// browser backend — candidates for adding `protection:
    /// cloudflare` to `sites.json` so future scans skip the failing
    /// HTTP probe. Prints a paste-ready table; does not modify
    /// anything.
    #[arg(long, requires = "doctor", help_heading = "Doctor")]
    suggest_protection: bool,

    /// With `--suggest-protection`: directory holding persisted scan
    /// JSON files. Defaults to `$XDG_CACHE_HOME/adler/scans/` (then
    /// `$HOME/.cache/adler/scans/`), the same path the web UI writes
    /// to.
    #[arg(
        long,
        value_name = "PATH",
        requires = "suggest_protection",
        help_heading = "Doctor"
    )]
    scans_dir: Option<PathBuf>,

    /// Scaffold a new site entry: probe this URL template (must contain
    /// `{username}`) with the given existing account and a nonsense one,
    /// derive a signature, and print a ready-to-paste JSON entry. Does not
    /// modify the registry. Combine with `--proxy` to probe from a clean IP.
    #[arg(long, value_name = "URL", help_heading = "Registry")]
    add_site: Option<String>,

    /// Site name for `--add-site` (defaults to the URL host).
    #[arg(
        long,
        value_name = "NAME",
        requires = "add_site",
        help_heading = "Registry"
    )]
    name: Option<String>,

    /// Monitor mode: scan fresh, diff the found accounts against the last
    /// run's snapshot, report new/removed ones, and save a fresh snapshot
    /// (under the cache dir). Pair with `--interval` to keep watching.
    #[arg(long, help_heading = "Batch & enrichment")]
    watch: bool,

    /// With `--watch`, re-scan every N seconds (continuous). One-shot if
    /// omitted (compose with cron yourself).
    #[arg(
        long,
        value_name = "SECS",
        requires = "watch",
        help_heading = "Batch & enrichment"
    )]
    interval: Option<u64>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text, help_heading = "Output")]
    format: OutputFormat,

    /// Override the embedded site list with a JSON file at this path.
    #[arg(long, value_name = "PATH", help_heading = "Registry")]
    sites: Option<PathBuf>,

    /// Exclude the WhatsMyName-derived supplementary registry from
    /// the scan. By default the WMN tranche is included for maximum
    /// coverage; pass `--no-wmn` when you specifically need an
    /// MIT-only data lineage (the WMN file is CC BY-SA 4.0, see
    /// `LICENSE-CC-BY-SA-4.0`). Conflicts with `--sites`.
    #[arg(long, conflicts_with = "sites", help_heading = "Registry")]
    no_wmn: bool,

    /// Only scan the top N most-popular sites, ordered by curated
    /// rank (lower number = more popular). Compatible with `--tag`
    /// etc. for further narrowing. Sites without a `popularity`
    /// rank are dropped — useful for fast checks of high-signal
    /// targets without scanning every long-tail forum.
    #[arg(long, value_name = "N", help_heading = "Filtering")]
    top: Option<u32>,

    /// Only check sites whose name contains this substring (case-insensitive).
    /// Repeatable; comma-separated values also accepted.
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "NAME",
        help_heading = "Filtering"
    )]
    only: Vec<String>,

    /// Exclude sites whose name contains this substring (case-insensitive).
    /// Repeatable; comma-separated values also accepted.
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "NAME",
        help_heading = "Filtering"
    )]
    exclude: Vec<String>,

    /// Per-request timeout in seconds.
    #[arg(
        long,
        default_value_t = 10,
        value_name = "SECS",
        help_heading = "Network"
    )]
    timeout: u64,

    /// Max in-flight site checks.
    #[arg(long, default_value_t = DEFAULT_CONCURRENCY, value_name = "N", help_heading = "Network")]
    concurrency: NonZeroUsize,

    /// Cap total requests/second across all hosts. Uncapped by default.
    #[arg(long, value_name = "RPS", help_heading = "Network")]
    max_rps: Option<NonZeroU32>,

    /// Retry attempts after a transient ban (429 / Cloudflare). Default 2.
    /// Set 0 to disable — useful for `--doctor`, where a ban should surface
    /// immediately rather than being retried.
    #[arg(long, default_value_t = 2, value_name = "N", help_heading = "Network")]
    max_retries: u32,

    /// Total scan deadline in seconds. Sites still in flight produce Uncertain outcomes.
    #[arg(long, value_name = "SECS", help_heading = "Network")]
    deadline: Option<u64>,

    /// Show every site, including the (usually many) `NotFound` ones.
    /// By default the text output shows only Found and Uncertain results.
    #[arg(long, help_heading = "Output")]
    all: bool,

    /// Under each result, print which signal(s) produced the verdict
    /// (e.g. `HTTP 404 (status_not_found)`). JSON always includes this.
    #[arg(long, help_heading = "Output")]
    explain: bool,

    /// Print only found account URLs, one per line; suppress the progress
    /// bar, summary, and hints. Ideal for scripting.
    #[arg(short, long, help_heading = "Output")]
    quiet: bool,

    /// When to colorize text output. `auto` (default) colors only an
    /// interactive terminal and honors the `NO_COLOR` environment variable.
    #[arg(long, value_enum, default_value_t = ColorChoice::Auto, value_name = "WHEN", help_heading = "Output")]
    color: ColorChoice,

    /// Disable the progress bar even on an interactive terminal.
    #[arg(long, help_heading = "Output")]
    no_progress: bool,

    /// Route all requests through a proxy (http://, https://, or socks5://).
    #[arg(
        long,
        value_name = "URL",
        conflicts_with = "tor",
        help_heading = "Network"
    )]
    proxy: Option<String>,

    /// Route geo / IP-type-specific sites through a pool of proxies
    /// defined in a TOML file (`[[egress]]` entries with `url`,
    /// optional `country` and `kind`). Only sites whose `access` policy
    /// requires a matching egress use the pool; everything else uses the
    /// default egress (`--proxy` or direct). See README → Egress pool.
    #[arg(long, value_name = "FILE", help_heading = "Access engine")]
    proxy_pool: Option<PathBuf>,

    /// Supply authenticated sessions from a TOML file. Each `[name]`
    /// table is a set of HTTP headers (e.g. `Cookie`, `Authorization`)
    /// applied to sites whose `access.session` names it — your own
    /// (sock-puppet) login, used to reach pages behind a login wall.
    /// Header values are secret: never logged. See README → Sessions.
    #[arg(long, value_name = "FILE", help_heading = "Access engine")]
    sessions: Option<PathBuf>,

    /// Route through a local Tor SOCKS proxy (`socks5://127.0.0.1:9050`).
    #[arg(long, help_heading = "Network")]
    tor: bool,

    /// Rotate the User-Agent header per request from a built-in browser pool.
    #[arg(long, help_heading = "Network")]
    rotate_ua: bool,

    /// Honor each site's robots.txt: skip probes to disallowed paths
    /// (reported Uncertain). Adds one cached robots.txt fetch per host.
    #[arg(long, help_heading = "Network")]
    respect_robots: bool,

    /// Browser backend used for sites tagged `bot-protected` (Instagram,
    /// X/Twitter, `TikTok`, Facebook, Threads, Snapchat, Weibo). `local`
    /// needs Chrome installed; `browserbase` reads
    /// `ADLER_BROWSERBASE_API_KEY` / `ADLER_BROWSERBASE_PROJECT_ID` and
    /// charges per session-minute. Default `none` leaves those sites on
    /// raw HTTP (typically Uncertain).
    #[arg(long, value_enum, default_value_t = BrowserBackendChoice::None, value_name = "BACKEND", help_heading = "Access engine")]
    browser_backend: BrowserBackendChoice,

    /// Per-scan cap on browser-routed probes. Once exceeded, remaining
    /// bot-protected sites return `Uncertain(browser_budget_exceeded)`.
    /// Guardrail against a misconfigured flag burning a whole quota.
    #[arg(long, default_value_t = adler_core::DEFAULT_BROWSER_BUDGET, value_name = "N", help_heading = "Access engine")]
    browser_budget: usize,

    /// Base URL of a self-hosted `FlareSolverr` instance (e.g.
    /// `http://localhost:8191`). Implies `--browser-backend
    /// flaresolverr` when set. Free alternative to Browserbase
    /// for Cloudflare-WAF sites; see the project README for
    /// `docker run` setup.
    #[arg(long, value_name = "URL", help_heading = "Access engine")]
    flaresolverr: Option<String>,

    /// Disable the browser backend for this run, even if `--browser-backend`
    /// or its env vars are set. Convenient for one-off raw-HTTP scans.
    #[arg(long, help_heading = "Access engine")]
    no_browser: bool,

    /// Per-scan cap on automatic escalations from the cheap transport
    /// (HTTP / impersonate) to the browser when the cheap path returns
    /// `Uncertain(cloudflare_challenge | rate_limited)`. Independent of
    /// `--browser-budget` so the pre-tagged `bot-protected` subset and the
    /// long-tail escalation subset don't fight over the same number.
    /// Defaults to `adler_core::DEFAULT_ESCALATION_BUDGET`.
    #[arg(long, default_value_t = adler_core::DEFAULT_ESCALATION_BUDGET, value_name = "N", help_heading = "Access engine")]
    escalation_budget: usize,

    /// Disable automatic escalation entirely — the cheap transport's
    /// outcome stands even when its `Uncertain` reason is one a browser
    /// fetch would resolve. Useful when benchmarking the raw HTTP signals
    /// or when you want strict cheap-path semantics.
    #[arg(long, help_heading = "Access engine")]
    no_escalation: bool,

    /// Extract profile fields (name, bio, avatar, …) from found accounts on
    /// sites that declare extractor rules. Implies a fresh scan (skips the
    /// cache) so enrichment data is current.
    #[arg(long, help_heading = "Batch & enrichment")]
    enrich: bool,

    /// Also search spelling variants of the username (separator swaps, leet,
    /// digit suffixes). Multiplies requests by the number of variants.
    #[arg(long, value_enum, default_value_t = Permute::None, value_name = "LEVEL", help_heading = "Batch & enrichment")]
    permute: Permute,

    /// Group found accounts that look like the same person (by name/bio
    /// similarity) and print the clusters. Implies `--enrich`.
    #[arg(long, help_heading = "Batch & enrichment")]
    correlate: bool,

    /// Skip the result cache for this run (no read, no write).
    #[arg(long, help_heading = "Cache")]
    no_cache: bool,

    /// Cache time-to-live in seconds. Entries older than this are ignored.
    #[arg(
        long,
        default_value_t = 3600,
        value_name = "SECS",
        help_heading = "Cache"
    )]
    cache_ttl: u64,

    /// Override the cache file location.
    #[arg(long, value_name = "PATH", help_heading = "Cache")]
    cache_path: Option<PathBuf>,

    /// Delete the cache file and exit.
    #[arg(long, help_heading = "Cache")]
    cache_clear: bool,

    /// Append an NDJSON record per result (ts, username, site, url, kind)
    /// to this file, for an accountable trail of what was queried.
    #[arg(long, value_name = "PATH", help_heading = "Output")]
    audit_log: Option<PathBuf>,

    /// Start the web UI server instead of running a scan. Binds to
    /// `127.0.0.1:8765` by default — override with `--web-bind`.
    /// Browse to <http://localhost:8765> once it starts.
    ///
    /// The server hosts a JSON + Server-Sent Events API that the
    /// `SolidJS` frontend (or any HTTP client) drives. Endpoint
    /// reference: <https://adler-docs.pages.dev/web-ui/#json-api>.
    #[arg(long, conflicts_with_all = [
        "watch", "input", "doctor", "list_sites", "list_tags",
        "completions", "add_site", "cache_clear", "correlate",
    ], help_heading = "Web UI")]
    web: bool,

    /// Address the web server listens on (`host:port`). Implies
    /// `--web`. Default `127.0.0.1:8765`. Binding a non-loopback
    /// address exposes the API without authentication — only set
    /// this on a trusted network.
    #[arg(long, value_name = "ADDR", requires = "web", help_heading = "Web UI")]
    web_bind: Option<SocketAddr>,
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
    /// Self-hosted `FlareSolverr` instance. Reads the endpoint URL
    /// from `--flaresolverr` (or `ADLER_FLARESOLVERR_URL`). Free,
    /// runs in Docker, targets Cloudflare-WAF sites.
    Flaresolverr,
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
    // `adler` with no arguments at all → friendly orientation, not
    // clap's terse "required arguments were not provided" error.
    // Every other invocation (typo, bad flag, missing value for an
    // explicit flag) still goes through clap's normal validation.
    if std::env::args_os().len() == 1 {
        print_quickstart();
        return ExitCode::SUCCESS;
    }
    let cli = Cli::parse();
    init_tracing();
    match run(cli).await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("adler: {err:#}");
            ExitCode::from(2)
        }
    }
}

/// Bare-`adler`-invocation orientation. Mirrors the web banner's
/// visual idiom (red brand, dim sub-text, TTY-aware).
fn print_quickstart() {
    let bin = std::env::args()
        .next()
        .as_deref()
        .and_then(|p| {
            std::path::Path::new(p)
                .file_name()
                .and_then(|f| f.to_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "adler".to_owned());
    let tty = io::stderr().is_terminal();
    let red = if tty { "\x1b[1;31m" } else { "" };
    let bold = if tty { "\x1b[1m" } else { "" };
    let dim = if tty { "\x1b[2m" } else { "" };
    let r = if tty { "\x1b[0m" } else { "" };

    eprintln!();
    eprintln!("  {red}ADLER{r}{dim}  ·  OSINT username search across many sites{r}");
    eprintln!();
    eprintln!("  {bold}Quick scan{r}");
    eprintln!("    {dim}${r} {bin} {bold}torvalds{r}");
    eprintln!();
    eprintln!("  {bold}Web UI{r}");
    eprintln!("    {dim}${r} {bin} {bold}--web{r}            {dim}# http://127.0.0.1:8765{r}");
    eprintln!();
    eprintln!("  {bold}Other modes{r}");
    eprintln!("    {dim}${r} {bin} --doctor           {dim}# registry health check{r}");
    eprintln!("    {dim}${r} {bin} --list-sites       {dim}# enumerate bundled sites{r}");
    eprintln!("    {dim}${r} {bin} --add-site <URL>   {dim}# scaffold a new site entry{r}");
    eprintln!();
    eprintln!("  {dim}See `{bin} --help` for filters, output formats, and the full flag list.{r}");
    eprintln!();
}

/// Install the stderr tracing subscriber.
fn init_tracing() {
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

    if cli.man_page {
        // clap_mangen renders a roff(1) man page from the clap Command
        // definition — single source of truth for both --help and the
        // generated man page.
        let cmd = Cli::command();
        clap_mangen::Man::new(cmd)
            .render(&mut io::stdout())
            .context("rendering man page")?;
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

    let registry = match (&cli.sites, cli.no_wmn) {
        (Some(path), _) => Registry::load_from_path(path)
            .with_context(|| format!("loading sites from {}", path.display()))?,
        (None, false) => Registry::default_embedded_with_wmn()
            .context("loading embedded registry + WhatsMyName tranche")?,
        (None, true) => Registry::default_embedded().context("loading embedded registry")?,
    };

    if cli.list_tags {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        for (tag, count) in registry.tag_counts() {
            writeln!(out, "{tag}\t{count}")?;
        }
        return Ok(ExitCode::SUCCESS);
    }

    let mut sites = registry.filter(
        &cli.only,
        &cli.exclude,
        &cli.tag,
        &cli.exclude_tag,
        cli.nsfw,
    );
    if let Some(n) = cli.top {
        // Restrict to ranked sites within the top N, ordered by
        // popularity (lower rank = more popular). Sites without a
        // populated `popularity` field are dropped — they have no
        // rank to compete with. Useful for fast checks: `adler
        // --top 30 alice` runs against the ~30 most-known sites
        // in seconds.
        sites.retain(|s| s.popularity.is_some_and(|p| p <= n));
        sites.sort_by_key(|s| s.popularity.unwrap_or(u32::MAX));
    }
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

    if cli.web {
        return run_web(&cli, sites, client).await;
    }

    if cli.doctor {
        let color = cli.color.resolve(io::stdout().is_terminal());
        let opts = DoctorOpts {
            fix: cli.fix,
            apply: cli.apply,
            yes: cli.yes,
            suggest_known_present: cli.suggest_known_present,
            suggest_protection: cli.suggest_protection,
            sites_path: cli.sites.as_deref(),
            scans_dir: cli.scans_dir.as_deref(),
            color,
        };
        return run_doctor(&client, &sites, opts).await;
    }

    run_scan(&cli, &client, &sites).await
}

/// `--web`: start the embedded HTTP API server and block until shutdown.
///
/// The site list and HTTP client are pre-built so the server honors
/// the same filtering / proxy / browser-backend flags the CLI exposes
/// for one-shot scans.
async fn run_web(cli: &Cli, sites: Vec<Site>, client: Client) -> Result<ExitCode> {
    let bind = cli
        .web_bind
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 8765)));
    let scans_dir = adler_server::default_scans_dir();
    let config = adler_server::AppConfig {
        bind,
        scan_capacity: 32,
        scans_dir: Some(scans_dir.clone()),
    };
    print_web_banner(bind, sites.len(), &scans_dir);
    adler_server::serve(sites, client, config)
        .await
        .context("running web server")?;
    Ok(ExitCode::SUCCESS)
}

/// Pretty boot banner. Falls back to plain ASCII when stderr isn't
/// a terminal (piped logs, CI capture) so we don't poison scrapers
/// with bare escape codes.
fn print_web_banner(bind: SocketAddr, site_count: usize, scans_dir: &std::path::Path) {
    let tty = io::stderr().is_terminal();
    let red = if tty { "\x1b[1;31m" } else { "" };
    let bold = if tty { "\x1b[1m" } else { "" };
    let dim = if tty { "\x1b[2m" } else { "" };
    let r = if tty { "\x1b[0m" } else { "" };

    let line_pad = "  ";
    eprintln!();
    eprintln!("{line_pad}{red}ADLER{r}{dim}  ·  OSINT username search{r}");
    eprintln!();
    eprintln!("{line_pad}{dim}→{r}  {bold}http://{bind}{r}");
    eprintln!(
        "{line_pad}{dim}→{r}  {} sites in catalogue",
        format_with_commas(site_count),
    );
    eprintln!(
        "{line_pad}{dim}→{r}  history at {}{}{}",
        dim,
        scans_dir.display(),
        r,
    );
    eprintln!();
    eprintln!("{line_pad}{dim}Ctrl-C to stop · ADLER_LOG=debug for verbose logs{r}");
    eprintln!();
}

/// Tiny "1234567 → 1,234,567" formatter — avoids pulling in a number
/// formatting crate for one call site.
fn format_with_commas(n: usize) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(bytes.len() + bytes.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(char::from(*b));
    }
    out
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

/// A proxy-pool config file (`--proxy-pool`): `[[egress]]` entries
/// describing the geo / IP-type-tagged proxies that sites can require
/// via their access policy.
#[derive(serde::Deserialize)]
struct ProxyPoolFile {
    #[serde(default)]
    egress: Vec<EgressSpec>,
}

/// Parse the TOML body of a proxy-pool file into egress specs.
fn parse_proxy_pool(text: &str) -> Result<Vec<EgressSpec>> {
    let parsed: ProxyPoolFile = toml::from_str(text).context("parsing proxy pool TOML")?;
    Ok(parsed.egress)
}

/// Read and parse a `--proxy-pool` file.
fn load_proxy_pool(path: &std::path::Path) -> Result<Vec<EgressSpec>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading proxy pool {}", path.display()))?;
    parse_proxy_pool(&text).with_context(|| format!("in proxy pool {}", path.display()))
}

/// Parse the TOML body of a `--sessions` file: each top-level `[name]`
/// table is a set of HTTP headers for that named session.
fn parse_sessions(text: &str) -> Result<SessionStore> {
    let raw: std::collections::HashMap<String, std::collections::BTreeMap<String, String>> =
        toml::from_str(text).context("parsing sessions TOML")?;
    let mut store = SessionStore::new();
    for (name, headers) in raw {
        store.insert(name, Session::from_headers(headers));
    }
    Ok(store)
}

/// Read and parse a `--sessions` file.
fn load_sessions(path: &std::path::Path) -> Result<SessionStore> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading sessions {}", path.display()))?;
    parse_sessions(&text).with_context(|| format!("in sessions {}", path.display()))
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
    if let Some(path) = &cli.proxy_pool {
        builder = builder.egress_pool(load_proxy_pool(path)?);
    }
    if let Some(path) = &cli.sessions {
        builder = builder.sessions(load_sessions(path)?);
    }
    if cli.rotate_ua {
        builder =
            builder.rotate_user_agents(USER_AGENT_POOL.iter().map(|s| (*s).to_owned()).collect());
    }

    if let Some(backend) = build_browser_backend(cli, proxy_for_browser.as_deref()).await? {
        builder = builder.browser(backend).browser_budget(cli.browser_budget);
    }

    builder = builder.escalation_budget(cli.escalation_budget);
    if cli.no_escalation {
        builder = builder.disable_escalation();
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
    // `--flaresolverr <URL>` is a shorthand for `--browser-backend
    // flaresolverr` plus the endpoint — if the user passed the URL
    // but not the explicit backend choice, promote it.
    let effective =
        if cli.flaresolverr.is_some() && cli.browser_backend == BrowserBackendChoice::None {
            BrowserBackendChoice::Flaresolverr
        } else {
            cli.browser_backend
        };
    match effective {
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
        BrowserBackendChoice::Flaresolverr => {
            let endpoint = cli
                .flaresolverr
                .clone()
                .or_else(|| std::env::var("ADLER_FLARESOLVERR_URL").ok())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "--browser-backend flaresolverr requires --flaresolverr <URL> or ADLER_FLARESOLVERR_URL env var"
                    )
                })?;
            let backend = adler_core::browser::FlareSolverrBackend::new(&endpoint)
                .context("connecting to FlareSolverr")?;
            eprintln!(
                "adler: routing bot-protected sites through FlareSolverr at {endpoint} (budget: {})",
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

// Internal CLI options struct — the variants are orthogonal independent
// toggles, not a state machine. The pedantic lint doesn't apply.
#[allow(clippy::struct_excessive_bools)]
struct DoctorOpts<'a> {
    fix: bool,
    apply: bool,
    yes: bool,
    suggest_known_present: bool,
    suggest_protection: bool,
    sites_path: Option<&'a Path>,
    scans_dir: Option<&'a Path>,
    color: bool,
}

async fn run_doctor(client: &Client, sites: &[Site], opts: DoctorOpts<'_>) -> Result<ExitCode> {
    tracing::info!(
        count = sites.len(),
        fix = opts.fix,
        apply = opts.apply,
        suggest_known_present = opts.suggest_known_present,
        "starting doctor"
    );
    let mut failures = 0_usize;
    let mut failed_sites: Vec<&Site> = Vec::new();
    for site in sites {
        let report = doctor::check_site(client, site).await;
        match report {
            DoctorReport::Healthy { .. } => {
                if opts.color {
                    println!("\x1b[32m[OK]\x1b[0m   {}", site.name);
                } else {
                    println!("[OK]   {}", site.name);
                }
            }
            DoctorReport::Unhealthy { issues, .. } => {
                failures += 1;
                failed_sites.push(site);
                if opts.color {
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

    if opts.fix && !failed_sites.is_empty() {
        if opts.apply {
            // `--apply` requires `--sites` (enforced by clap), so this is
            // always Some by construction; the `?` is just belt-and-braces.
            let path = opts
                .sites_path
                .context("internal: --apply reached run_doctor without --sites")?;
            apply_fix_suggestions(client, &failed_sites, path, opts.yes).await?;
        } else {
            print_fix_suggestions(client, &failed_sites).await?;
        }
    }

    if opts.suggest_known_present && !failed_sites.is_empty() {
        print_known_present_suggestions(client, &failed_sites).await?;
    }

    if opts.suggest_protection {
        // Scope-independent of the site-health check above: this draws
        // on persisted scan history, not on a live registry probe.
        print_protection_suggestions(opts.scans_dir);
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

/// `--apply` variant: collect suggestions, render a per-site signal diff,
/// confirm once (unless `--yes`), then write the patched JSON back atomically.
///
/// Behaviour invariants:
/// - Sites for which `suggest_fix` returns `None` are skipped, not patched
///   with empty signals.
/// - A site missing from the JSON file (e.g. registry merged from a tranche
///   not on disk) is reported and skipped, not erased.
/// - Atomic rename through a sibling `*.tmp` file means a crash mid-write
///   leaves the original intact.
async fn apply_fix_suggestions(
    client: &Client,
    failed: &[&Site],
    sites_path: &Path,
    skip_prompt: bool,
) -> Result<()> {
    let mut fixes: Vec<(String, Vec<adler_core::Signal>, String)> = Vec::new();
    println!(
        "\ngathering fix suggestions for {} failing site(s)…",
        failed.len()
    );
    for site in failed {
        if let Some(fix) = doctor::suggest_fix(client, site).await {
            fixes.push((site.name.clone(), fix.signals, fix.rationale));
        } else {
            println!(
                "  {}  — skipped (no suggestion; responses indistinguishable)",
                site.name
            );
        }
    }
    if fixes.is_empty() {
        println!("\nno applicable fixes — nothing to write.");
        return Ok(());
    }

    let in_memory: std::collections::HashMap<&str, &Site> =
        failed.iter().map(|s| (s.name.as_str(), *s)).collect();
    println!("\nproposed changes:");
    for (name, signals, rationale) in &fixes {
        println!("\n  {name}  ({rationale})");
        if let Some(site) = in_memory.get(name.as_str()) {
            for old in &site.signals {
                println!("    - {}", render_signal(old));
            }
        }
        for new in signals {
            println!("    + {}", render_signal(new));
        }
    }
    println!(
        "\n{} site(s) to patch in {}",
        fixes.len(),
        sites_path.display()
    );

    if !skip_prompt {
        print!("Apply? [y/N] ");
        io::stdout().flush().ok();
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .context("reading confirmation prompt")?;
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "YES") {
            println!("aborted; no changes written.");
            return Ok(());
        }
    }

    let patches: Vec<(String, Vec<adler_core::Signal>)> =
        fixes.into_iter().map(|(n, s, _)| (n, s)).collect();
    let report = patch_sites_file(sites_path, &patches)?;

    if !report.missing.is_empty() {
        println!(
            "warning: {} site(s) had a suggestion but no matching entry in {}: {}",
            report.missing.len(),
            sites_path.display(),
            report.missing.join(", ")
        );
    }

    println!(
        "patched {} site(s) in {}; re-run --doctor to verify.",
        report.patched,
        sites_path.display()
    );
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
struct PatchReport {
    /// How many site entries were updated.
    patched: usize,
    /// Names that had a suggestion but no matching JSON entry — skipped,
    /// not erased.
    missing: Vec<String>,
}

/// Pure helper: read the JSON, replace `signals` on matching entries by
/// name, write the result back atomically through a sibling `*.tmp` file.
/// Split out from [`apply_fix_suggestions`] so it can be unit-tested
/// without a [`Client`] or the network.
fn patch_sites_file(
    sites_path: &Path,
    patches: &[(String, Vec<adler_core::Signal>)],
) -> Result<PatchReport> {
    let content = std::fs::read_to_string(sites_path)
        .with_context(|| format!("reading {} for --apply", sites_path.display()))?;
    let mut root: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("parsing {} as JSON", sites_path.display()))?;
    let arr = root
        .get_mut("sites")
        .and_then(serde_json::Value::as_array_mut)
        .with_context(|| {
            format!(
                "{} has no top-level \"sites\" array — is it a valid registry file?",
                sites_path.display()
            )
        })?;

    let mut report = PatchReport::default();
    for (name, signals) in patches {
        let entry = arr.iter_mut().find_map(|v| {
            let obj = v.as_object_mut()?;
            (obj.get("name").and_then(serde_json::Value::as_str) == Some(name.as_str()))
                .then_some(obj)
        });
        match entry {
            Some(obj) => {
                obj.insert("signals".into(), serde_json::to_value(signals)?);
                report.patched += 1;
            }
            None => report.missing.push(name.clone()),
        }
    }

    let mut serialised =
        serde_json::to_string_pretty(&root).context("re-serialising patched registry")?;
    serialised.push('\n');

    let tmp = sites_path.with_extension("json.tmp");
    std::fs::write(&tmp, serialised.as_bytes())
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, sites_path)
        .with_context(|| format!("renaming {} → {}", tmp.display(), sites_path.display()))?;

    Ok(report)
}

/// Render a [`Signal`] in compact JSON for the diff output. Falls back to
/// the `Debug` impl on the (impossible) serialisation failure so the diff
/// always has something to show.
fn render_signal(s: &adler_core::Signal) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}"))
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

/// Default directory the web UI persists scans to (`$XDG_CACHE_HOME/adler/scans/`,
/// fallback `$HOME/.cache/adler/scans/`). Mirrors `adler_server::persist::default_dir`
/// — duplicated here so adler-cli doesn't take a dep on adler-server for one path.
fn default_scans_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(xdg).join("adler").join("scans");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".cache")
            .join("adler")
            .join("scans");
    }
    PathBuf::from("adler-scans")
}

/// `--suggest-protection`: walk the persisted scan history, group
/// `CheckOutcome`s by site, and surface sites that consistently escalated
/// through the browser backend. Each finding is a paste-ready candidate
/// for adding `protection: cloudflare` to `sites.json`.
///
/// The on-disk scan format is owned by `adler-server`; we parse only the
/// `outcomes` field here so the CLI doesn't take a dependency on
/// adler-server's full `PersistedScan` shape.
fn print_protection_suggestions(scans_dir: Option<&Path>) {
    #[derive(serde::Deserialize)]
    struct PersistedScanLite {
        outcomes: Vec<CheckOutcome>,
    }

    let dir = scans_dir.map_or_else(default_scans_dir, Path::to_path_buf);
    println!("\ntelemetry suggestions (reading {} ):", dir.display());

    let read_dir = match std::fs::read_dir(&dir) {
        Ok(it) => it,
        Err(e) => {
            println!(
                "  cannot read {}: {e}. Either no scans persisted yet (run `adler --web` \
                 and let it record some), or pass --scans-dir <path>.",
                dir.display()
            );
            return;
        }
    };

    let mut scans: Vec<Vec<CheckOutcome>> = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(scan) = serde_json::from_slice::<PersistedScanLite>(&bytes) else {
            continue;
        };
        scans.push(scan.outcomes);
    }

    if scans.is_empty() {
        println!(
            "  no parseable scans found in {}. Re-run after `adler --web` has recorded a few.",
            dir.display()
        );
        return;
    }

    let slices: Vec<&[CheckOutcome]> = scans.iter().map(Vec::as_slice).collect();
    let findings = adler_core::telemetry::analyze_escalation_history(
        slices.iter().copied(),
        adler_core::telemetry::DEFAULT_THRESHOLD_RATIO,
        adler_core::telemetry::DEFAULT_MIN_SCANS,
    );

    println!(
        "  scanned {} persisted scan(s); threshold ≥{:.0}% over ≥{} scans.\n",
        scans.len(),
        adler_core::telemetry::DEFAULT_THRESHOLD_RATIO * 100.0,
        adler_core::telemetry::DEFAULT_MIN_SCANS,
    );

    if findings.is_empty() {
        println!("  no sites met the suggest-protection threshold.");
        return;
    }

    println!(
        "  {:<32}  {:>6}  {:>10}  {:>7}  suggested",
        "site", "scans", "escalated", "ratio"
    );
    for f in &findings {
        println!(
            "  {:<32}  {:>6}  {:>10}  {:>6.1}%  protection: {:?}",
            f.site,
            f.scans_seen,
            f.escalation_evidence,
            f.ratio() * 100.0,
            f.suggested_protection,
        );
    }
    println!(
        "\n  {} site(s) suggested. Paste-ready snippet:",
        findings.len()
    );
    println!("\nPROTECTION additions:");
    for f in &findings {
        // protection is serialized kebab-case (e.g. `cloudflare`, `cf-firewall`).
        let kind = serde_json::to_string(&f.suggested_protection)
            .unwrap_or_else(|_| format!("{:?}", f.suggested_protection));
        println!("  {:?}: {{\"protection\": [{}]}},", f.site, kind);
    }
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
    use adler_core::{CorrelationReport, EgressKind, UncertainReason};
    use std::collections::BTreeMap;

    #[test]
    fn parses_proxy_pool_toml() {
        let toml = r#"
            [[egress]]
            url = "socks5://pl.example:1080"
            country = "PL"
            kind = "residential"

            [[egress]]
            url = "http://dc.example:8080"
        "#;
        let specs = parse_proxy_pool(toml).expect("parses");
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].country.as_ref().unwrap().as_str(), "pl");
        assert!(matches!(specs[0].kind, EgressKind::Residential));
        // Second entry omits country/kind → None + default Datacenter.
        assert!(specs[1].country.is_none());
        assert!(matches!(specs[1].kind, EgressKind::Datacenter));
    }

    #[test]
    fn empty_proxy_pool_toml_is_ok() {
        assert!(parse_proxy_pool("").expect("parses").is_empty());
    }

    #[test]
    fn parses_sessions_toml() {
        let toml = r#"
            [ig]
            Cookie = "sessionid=abc"
            X-CSRF-Token = "tok"

            [reddit]
            Cookie = "reddit_session=xyz"
        "#;
        let store = parse_sessions(toml).expect("parses");
        assert_eq!(store.len(), 2);
        assert!(!store.is_empty());
    }

    #[test]
    fn empty_sessions_toml_is_ok() {
        assert!(parse_sessions("").expect("parses").is_empty());
    }

    fn outcome(site: &str, kind: MatchKind) -> CheckOutcome {
        CheckOutcome {
            site: site.into(),
            url: format!("https://{site}.example/u"),
            kind,
            reason: None,
            elapsed_ms: 1,
            enrichment: BTreeMap::new(),
            evidence: Vec::new(),
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

    #[test]
    fn patch_sites_file_replaces_signals_in_place_and_preserves_other_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sites.json");
        std::fs::write(
            &path,
            r#"{
  "_comment": "preserve me",
  "engines": {
    "Discourse": {"signals": [{"kind": "status_found", "codes": [200]}]}
  },
  "sites": [
    {
      "name": "github.example",
      "url": "https://gh.example/{username}",
      "tags": ["dev", "source:custom"],
      "known_present": "torvalds",
      "signals": [{"kind": "status_found", "codes": [200]}]
    },
    {
      "name": "uses-engine.example",
      "url": "https://ue.example/{username}",
      "engine": "Discourse",
      "tags": ["forum"]
    }
  ]
}"#,
        )
        .unwrap();

        let patches = vec![
            (
                "github.example".to_owned(),
                vec![
                    adler_core::Signal::StatusFound { codes: vec![200] },
                    adler_core::Signal::StatusNotFound { codes: vec![404] },
                ],
            ),
            (
                "uses-engine.example".to_owned(),
                vec![adler_core::Signal::BodyAbsent {
                    text: "User not found".to_owned(),
                }],
            ),
            (
                "never-existed.example".to_owned(),
                vec![adler_core::Signal::StatusFound { codes: vec![200] }],
            ),
        ];

        let report = patch_sites_file(&path, &patches).expect("patch ok");
        assert_eq!(report.patched, 2);
        assert_eq!(report.missing, vec!["never-existed.example".to_owned()]);

        let written = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&written).unwrap();

        // Top-level fields preserved.
        assert_eq!(v["_comment"], "preserve me");
        assert!(v["engines"]["Discourse"]["signals"].is_array());

        let arr = v["sites"].as_array().unwrap();
        assert_eq!(arr.len(), 2, "no entries added or removed");

        let gh = arr.iter().find(|s| s["name"] == "github.example").unwrap();
        assert_eq!(gh["url"], "https://gh.example/{username}");
        assert_eq!(gh["known_present"], "torvalds");
        assert_eq!(gh["tags"], serde_json::json!(["dev", "source:custom"]));
        // signals replaced — now has two entries.
        let signals = gh["signals"].as_array().unwrap();
        assert_eq!(signals.len(), 2);
        assert_eq!(signals[1]["kind"], "status_not_found");
        assert_eq!(signals[1]["codes"], serde_json::json!([404]));

        let ue = arr
            .iter()
            .find(|s| s["name"] == "uses-engine.example")
            .unwrap();
        // engine reference preserved alongside the new explicit signals.
        assert_eq!(ue["engine"], "Discourse");
        let ue_signals = ue["signals"].as_array().unwrap();
        assert_eq!(ue_signals.len(), 1);
        assert_eq!(ue_signals[0]["kind"], "body_absent");
        assert_eq!(ue_signals[0]["text"], "User not found");

        // Atomic rename means no stray *.tmp left behind.
        let tmp_path = path.with_extension("json.tmp");
        assert!(!tmp_path.exists());
    }

    #[test]
    fn patch_sites_file_errors_on_missing_sites_array() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sites.json");
        std::fs::write(&path, r#"{"engines": {}}"#).unwrap();

        let patches = vec![(
            "any.example".to_owned(),
            vec![adler_core::Signal::StatusFound { codes: vec![200] }],
        )];
        let err = patch_sites_file(&path, &patches).unwrap_err();
        assert!(
            err.to_string().contains("no top-level \"sites\" array"),
            "unexpected error: {err}",
        );
    }
}
