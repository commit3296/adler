//! Adler CLI entry point.

mod doctor;
mod output;
mod report;
mod scan;
mod transport;

use adler_core::{Cache, Client, PermuteLevel, Registry, Site};
use std::io::{self, IsTerminal as _, Write};
use std::net::SocketAddr;
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::PathBuf;
use std::process::ExitCode;

use crate::doctor::{DoctorOpts, run_doctor};
use crate::scan::run_scan;
use crate::transport::build_client;
use anyhow::{Context as _, Result};
use clap::{CommandFactory as _, Parser, ValueEnum};
use clap_complete::Shell;
use tracing_subscriber::{EnvFilter, fmt};

pub(crate) const DEFAULT_CONCURRENCY: NonZeroUsize = match NonZeroUsize::new(32) {
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
pub(crate) struct Cli {
    /// Username to search for. With `--add-site`, this is an account that
    /// EXISTS on the site (used to derive the signature). Not required with
    /// `--doctor`, `--cache-clear`, `--list-sites`, or `--completions`.
    #[arg(required_unless_present_any = ["doctor", "cache_clear", "list_sites", "list_tags", "completions", "man_page", "add_site", "input", "web", "mcp", "mcp_http"])]
    pub(crate) username: Option<String>,

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

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text, help_heading = "Output")]
    pub(crate) format: OutputFormat,

    /// Show every site, including the (usually many) `NotFound` ones.
    /// By default the text output shows only Found and Uncertain results.
    #[arg(long, help_heading = "Output")]
    pub(crate) all: bool,

    /// Under each result, print which signal(s) produced the verdict
    /// (e.g. `HTTP 404 (status_not_found)`). JSON always includes this.
    #[arg(long, help_heading = "Output")]
    pub(crate) explain: bool,

    /// Print only found account URLs, one per line; suppress the progress
    /// bar, summary, and hints. Ideal for scripting.
    #[arg(short, long, help_heading = "Output")]
    pub(crate) quiet: bool,

    /// When to colorize text output. `auto` (default) colors only an
    /// interactive terminal and honors the `NO_COLOR` environment variable.
    #[arg(long, value_enum, default_value_t = ColorChoice::Auto, value_name = "WHEN", help_heading = "Output")]
    pub(crate) color: ColorChoice,

    /// Disable the progress bar even on an interactive terminal.
    #[arg(long, help_heading = "Output")]
    pub(crate) no_progress: bool,

    /// Append an NDJSON record per result (ts, username, site, url, kind)
    /// to this file, for an accountable trail of what was queried.
    #[arg(long, value_name = "PATH", help_heading = "Output")]
    pub(crate) audit_log: Option<PathBuf>,

    /// Per-request timeout in seconds.
    #[arg(
        long,
        default_value_t = 10,
        value_name = "SECS",
        help_heading = "Network"
    )]
    pub(crate) timeout: u64,

    /// Max in-flight site checks.
    #[arg(long, default_value_t = DEFAULT_CONCURRENCY, value_name = "N", help_heading = "Network")]
    pub(crate) concurrency: NonZeroUsize,

    /// Cap total requests/second across all hosts. Uncapped by default.
    #[arg(long, value_name = "RPS", help_heading = "Network")]
    pub(crate) max_rps: Option<NonZeroU32>,

    /// Retry attempts after a transient ban (429 / Cloudflare). Default 2.
    /// Set 0 to disable — useful for `--doctor`, where a ban should surface
    /// immediately rather than being retried.
    #[arg(long, default_value_t = 2, value_name = "N", help_heading = "Network")]
    pub(crate) max_retries: u32,

    /// Total scan deadline in seconds. Sites still in flight produce Uncertain outcomes.
    #[arg(long, value_name = "SECS", help_heading = "Network")]
    pub(crate) deadline: Option<u64>,

    /// Route all requests through a proxy (http://, https://, or socks5://).
    #[arg(
        long,
        value_name = "URL",
        conflicts_with = "tor",
        help_heading = "Network"
    )]
    pub(crate) proxy: Option<String>,

    /// Route through a local Tor SOCKS proxy (`socks5://127.0.0.1:9050`).
    #[arg(long, help_heading = "Network")]
    pub(crate) tor: bool,

    /// Rotate the User-Agent header per request from a built-in browser pool.
    #[arg(long, help_heading = "Network")]
    pub(crate) rotate_ua: bool,

    /// Honor each site's robots.txt: skip probes to disallowed paths
    /// (reported Uncertain). Adds one cached robots.txt fetch per host.
    #[arg(long, help_heading = "Network")]
    pub(crate) respect_robots: bool,

    /// Route geo / IP-type-specific sites through a pool of proxies
    /// defined in a TOML file (`[[egress]]` entries with `url`,
    /// optional `country` and `kind`). Only sites whose `access` policy
    /// requires a matching egress use the pool; everything else uses the
    /// default egress (`--proxy` or direct). See README → Egress pool.
    #[arg(long, value_name = "FILE", help_heading = "Access engine")]
    pub(crate) proxy_pool: Option<PathBuf>,

    /// Supply authenticated sessions from a TOML file. Each `[name]`
    /// table is a set of HTTP headers (e.g. `Cookie`, `Authorization`)
    /// applied to sites whose `access.session` names it — your own
    /// (sock-puppet) login, used to reach pages behind a login wall.
    /// Header values are secret: never logged. See README → Sessions.
    #[arg(long, value_name = "FILE", help_heading = "Access engine")]
    pub(crate) sessions: Option<PathBuf>,

    /// Browser backend used for sites tagged `bot-protected` (Instagram,
    /// X/Twitter, `TikTok`, Facebook, Threads, Snapchat, Weibo). `local`
    /// needs Chrome installed; `browserbase` reads
    /// `ADLER_BROWSERBASE_API_KEY` / `ADLER_BROWSERBASE_PROJECT_ID` and
    /// charges per session-minute. Default `none` leaves those sites on
    /// raw HTTP (typically Uncertain).
    #[arg(long, value_enum, default_value_t = BrowserBackendChoice::None, value_name = "BACKEND", help_heading = "Access engine")]
    pub(crate) browser_backend: BrowserBackendChoice,

    /// Per-scan cap on browser-routed probes. Once exceeded, remaining
    /// bot-protected sites return `Uncertain(browser_budget_exceeded)`.
    /// Guardrail against a misconfigured flag burning a whole quota.
    #[arg(long, default_value_t = adler_core::DEFAULT_BROWSER_BUDGET, value_name = "N", help_heading = "Access engine")]
    pub(crate) browser_budget: usize,

    /// Base URL of a self-hosted `FlareSolverr` instance (e.g.
    /// `http://localhost:8191`). Implies `--browser-backend
    /// flaresolverr` when set. Free alternative to Browserbase
    /// for Cloudflare-WAF sites; see the project README for
    /// `docker run` setup.
    #[arg(long, value_name = "URL", help_heading = "Access engine")]
    pub(crate) flaresolverr: Option<String>,

    /// Disable the browser backend for this run, even if `--browser-backend`
    /// or its env vars are set. Convenient for one-off raw-HTTP scans.
    #[arg(long, help_heading = "Access engine")]
    pub(crate) no_browser: bool,

    /// Per-scan cap on automatic escalations from the cheap transport
    /// (HTTP / impersonate) to the browser when the cheap path returns
    /// `Uncertain(cloudflare_challenge | rate_limited)`. Independent of
    /// `--browser-budget` so the pre-tagged `bot-protected` subset and the
    /// long-tail escalation subset don't fight over the same number.
    /// Defaults to `adler_core::DEFAULT_ESCALATION_BUDGET`.
    #[arg(long, default_value_t = adler_core::DEFAULT_ESCALATION_BUDGET, value_name = "N", help_heading = "Access engine")]
    pub(crate) escalation_budget: usize,

    /// Disable automatic escalation entirely — the cheap transport's
    /// outcome stands even when its `Uncertain` reason is one a browser
    /// fetch would resolve. Useful when benchmarking the raw HTTP signals
    /// or when you want strict cheap-path semantics.
    #[arg(long, help_heading = "Access engine")]
    pub(crate) no_escalation: bool,

    /// Skip the result cache for this run (no read, no write).
    #[arg(long, help_heading = "Cache")]
    pub(crate) no_cache: bool,

    /// Cache time-to-live in seconds. Entries older than this are ignored.
    #[arg(
        long,
        default_value_t = 3600,
        value_name = "SECS",
        help_heading = "Cache"
    )]
    pub(crate) cache_ttl: u64,

    /// Override the cache file location.
    #[arg(long, value_name = "PATH", help_heading = "Cache")]
    pub(crate) cache_path: Option<PathBuf>,

    /// Delete the cache file and exit.
    #[arg(long, help_heading = "Cache")]
    cache_clear: bool,

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

    /// With `--doctor`: patch the file passed via `--sites` in place
    /// with whichever doctor suggestion mode is active (atomic write).
    /// Pair with `--fix` to apply the per-site signal suggestions,
    /// with `--suggest-known-present` to apply discovered replacement
    /// `known_present` users to stale entries, or with
    /// `--suggest-extract` to write derived `extract` blocks for sites
    /// that currently expose none. The embedded registry is read-only —
    /// pass `--sites <path>` to a writable JSON file. By default,
    /// prompts once after printing the diff; pass `--yes` to skip the
    /// prompt for non-interactive use.
    #[arg(long, requires = "sites", help_heading = "Doctor")]
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

    /// With `--doctor`: for each *healthy* site that doesn't yet declare
    /// any `extract` rules, fetch the `known_present` profile page and
    /// derive candidate selectors from its `OpenGraph` (`og:title` /
    /// `og:description` / `og:image`) and Twitter Card meta tags. Prints
    /// a paste-ready `extract` block per discovered site. Pair with
    /// `--apply --sites <path>` to write the discovered blocks back to
    /// the registry file. Does not modify anything on its own.
    #[arg(long, requires = "doctor", help_heading = "Doctor")]
    suggest_extract: bool,

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

    /// Scan every username in this file (one per line; blank lines and lines
    /// starting with `#` are skipped, duplicates removed). A positional
    /// username, if given, is scanned too. Output is grouped per username;
    /// not compatible with `--correlate` / `--format html`.
    #[arg(long, value_name = "PATH", help_heading = "Batch & enrichment")]
    pub(crate) input: Option<PathBuf>,

    /// Monitor mode: scan fresh, diff the found accounts against the last
    /// run's snapshot, report new/removed ones, and save a fresh snapshot
    /// (under the cache dir). Pair with `--interval` to keep watching.
    #[arg(long, help_heading = "Batch & enrichment")]
    pub(crate) watch: bool,

    /// With `--watch`, re-scan every N seconds (continuous). One-shot if
    /// omitted (compose with cron yourself).
    #[arg(
        long,
        value_name = "SECS",
        requires = "watch",
        help_heading = "Batch & enrichment"
    )]
    pub(crate) interval: Option<u64>,

    /// Extract profile fields (name, bio, avatar, …) from found accounts on
    /// sites that declare extractor rules. Implies a fresh scan (skips the
    /// cache) so enrichment data is current.
    #[arg(long, help_heading = "Batch & enrichment")]
    pub(crate) enrich: bool,

    /// Also search spelling variants of the username (separator swaps, leet,
    /// digit suffixes). Multiplies requests by the number of variants.
    #[arg(long, value_enum, default_value_t = Permute::None, value_name = "LEVEL", help_heading = "Batch & enrichment")]
    pub(crate) permute: Permute,

    /// Group found accounts that look like the same person (by name/bio
    /// similarity) and print the clusters. Implies `--enrich`.
    #[arg(long, help_heading = "Batch & enrichment")]
    pub(crate) correlate: bool,

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

    /// Start a Model Context Protocol (MCP) server over stdio
    /// instead of running a scan. Intended for AI assistants like
    /// Claude Desktop / Cursor / any agent that speaks MCP. The
    /// server exposes five tools (`list_sites`, `scan_username`
    /// with streaming progress, `scan_batch`, `doctor_check`,
    /// `get_scan_history`), five resources
    /// (`adler://registry/{sites,tags,disabled}`,
    /// `adler://scans/recent`, `adler://scans/{id}` template), and
    /// three prompts (`investigate_username`,
    /// `audit_registry_health`, `correlate_accounts`). Tracing output
    /// is forced onto stderr so stdout stays clean for the JSON-RPC
    /// protocol stream.
    #[arg(long, conflicts_with_all = [
        "watch", "input", "doctor", "list_sites", "list_tags",
        "completions", "add_site", "cache_clear", "correlate", "web",
    ], help_heading = "MCP")]
    mcp: bool,

    /// Start an MCP server over HTTP+SSE on this address (e.g.
    /// `127.0.0.1:8766`). Implies `--mcp` but switches the transport
    /// from stdio to the Streamable HTTP variant. The endpoint is
    /// mounted at `/mcp`, so an agent connects to
    /// `http://<addr>/mcp`. Default loopback-only hostname filter
    /// guards against DNS-rebind attacks; binding a non-loopback
    /// address exposes the API without authentication — only do it
    /// on a trusted network.
    #[arg(
        long,
        value_name = "ADDR",
        conflicts_with_all = [
            "watch", "input", "doctor", "list_sites", "list_tags",
            "completions", "add_site", "cache_clear", "correlate", "web",
        ],
        help_heading = "MCP",
    )]
    mcp_http: Option<SocketAddr>,

    /// Print a shell completion script to stdout and exit.
    #[arg(long, value_enum, value_name = "SHELL", help_heading = "Misc")]
    completions: Option<Shell>,

    /// Print a roff(1) man page to stdout and exit. Intended for distro
    /// packagers: `adler --man-page > /usr/share/man/man1/adler.1`.
    #[arg(long, help_heading = "Misc")]
    man_page: bool,
}

/// CLI mirror of [`PermuteLevel`] so clap parses it without coupling the
/// core type to clap.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum Permute {
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
pub(crate) enum OutputFormat {
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
pub(crate) enum BrowserBackendChoice {
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
pub(crate) enum ColorChoice {
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
    pub(crate) fn resolve(self, is_tty: bool) -> bool {
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

pub(crate) fn cache_path(cli: &Cli) -> PathBuf {
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

    if cli.mcp || cli.mcp_http.is_some() {
        return run_mcp(cli.mcp_http).await;
    }

    if cli.doctor {
        let color = cli.color.resolve(io::stdout().is_terminal());
        let opts = DoctorOpts {
            fix: cli.fix,
            apply: cli.apply,
            yes: cli.yes,
            suggest_known_present: cli.suggest_known_present,
            suggest_extract: cli.suggest_extract,
            suggest_protection: cli.suggest_protection,
            sites_path: cli.sites.as_deref(),
            scans_dir: cli.scans_dir.as_deref(),
            color,
            format: cli.format,
        };
        return run_doctor(&client, &sites, opts).await;
    }

    run_scan(&cli, &client, &sites).await
}

/// `--mcp` / `--mcp-http`: start the MCP server. When `http_bind` is
/// `Some`, listens for HTTP+SSE on that address (mounted at `/mcp`);
/// otherwise drives the stdio transport.
///
/// The server uses the default embedded registry; the CLI's
/// `--sites` / `--only` / etc. don't propagate yet because tools
/// declare their own filter parameters via MCP arguments. Stdout is
/// reserved for the JSON-RPC stream in stdio mode — boot banners,
/// tracing, and errors all go to stderr so the protocol stays clean.
async fn run_mcp(http_bind: Option<SocketAddr>) -> Result<ExitCode> {
    let server = adler_mcp::AdlerMcp::new().context("building adler-mcp server")?;
    if let Some(addr) = http_bind {
        eprintln!(
            "adler-mcp v{} — http+sse transport, endpoint http://{addr}{}, registry: {} sites",
            env!("CARGO_PKG_VERSION"),
            adler_mcp::HTTP_ENDPOINT,
            server.registry().len(),
        );
        adler_mcp::run_http(server, addr)
            .await
            .context("running mcp http server")?;
    } else {
        eprintln!(
            "adler-mcp v{} — stdio transport, registry: {} sites",
            env!("CARGO_PKG_VERSION"),
            server.registry().len(),
        );
        adler_mcp::run_stdio(server)
            .await
            .context("running mcp stdio server")?;
    }
    Ok(ExitCode::SUCCESS)
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

    let scaffold = adler_core::doctor::scaffold_site(client, &name, url, known)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_name_titlecases_host_label() {
        assert_eq!(derive_name("https://www.example.com/{username}"), "Example");
        assert_eq!(derive_name("https://github.com/{username}"), "Github");
        assert_eq!(derive_name("http://sub.example.co.uk/u/{username}"), "Sub");
        assert_eq!(derive_name("not a url"), "Not a url");
    }

    #[test]
    fn color_choice_resolves_against_tty_and_no_color() {
        assert!(ColorChoice::Always.resolve(false));
        assert!(!ColorChoice::Never.resolve(true));
        // Auto depends on TTY; NO_COLOR handling is covered by the env check
        // in `resolve` itself (not exercised here to avoid mutating env).
        assert!(!ColorChoice::Auto.resolve(false));
    }
}
