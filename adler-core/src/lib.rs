//! Core engine for the [Adler](https://github.com/commit3296/adler)
//! OSINT username-search tool — runtime-agnostic, embed-friendly.
//!
//! The CLI lives in `adler-cli`; this crate is what you reach for to
//! drive username detection from your own Rust code (a Discord bot
//! that checks usernames, a security tool that flags exposed
//! identities across a watchlist, a CI gate that asserts a name
//! isn't claimed elsewhere, …).
//!
//! ## Quick start
//!
//! Scan the embedded 1,900-entry main registry for one username and print
//! the hits:
//!
//! ```no_run
//! use adler_core::{Client, ExecutorOptions, MatchKind, Registry, Username, executor};
//!
//! # async fn run() -> adler_core::Result<()> {
//! let registry = Registry::default_embedded()?;
//!
//! // filter(include, exclude, tags, exclude_tags, include_nsfw)
//! // — empty slices = no name/tag filter; `false` keeps the
//! // default NSFW auto-exclusion (matches Sherlock's `--nsfw`
//! // opt-in). Pass `true` (or `&["nsfw".into()]` as tags) to
//! // scan adult-content sites.
//! let sites = registry.filter(&[], &[], &[], &[], false);
//!
//! let username = Username::new("torvalds")?;
//! let client = Client::builder().build()?;
//!
//! let outcomes =
//!     executor::run(&client, &sites, &username, ExecutorOptions::default()).await;
//!
//! for outcome in outcomes.iter().filter(|o| o.kind == MatchKind::Found) {
//!     println!("{} → {}", outcome.site, outcome.url);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## Map of the public API
//!
//! Detection plumbing:
//!
//! - [`Registry`] — loaded, validated collection of sites. Build from
//!   the embedded [`default_embedded`](Registry::default_embedded),
//!   from a JSON string ([`from_json_str`](Registry::from_json_str)),
//!   or from disk ([`load_from_path`](Registry::load_from_path)).
//! - [`Site`], [`Signal`], [`UrlTemplate`], [`Extractor`],
//!   [`KnownPresent`] — site-registry value types. `Site` is
//!   serde-(de)serialisable; the JSON Schema lives in `docs/sites.schema.json`.
//! - [`Username`] — validated search target. Constructed via
//!   [`Username::new`](Username::new); invalid characters / overlong
//!   names are rejected at construction time.
//! - [`Client`], [`ClientBuilder`] — `reqwest`-backed probe issuer.
//!   Knobs the builder exposes: timeout, redirect limit, per-host /
//!   global throttle, retry policy, user-agent rotation pool, proxy,
//!   `robots.txt` cache, browser backend, browser budget.
//! - [`CheckOutcome`], [`MatchKind`], [`UncertainReason`] — verdict
//!   types. The signal pipeline is *negative-priority*: any
//!   `NotFound` vote wins over `Found`; no votes → `Uncertain`. A
//!   per-site `regex_check` mismatch short-circuits with
//!   [`UncertainReason::UsernameNotAllowed`] before any HTTP request.
//! - [`executor`] — bounded-concurrency fan-out runner. Pass an
//!   [`ExecutorOptions`] to control concurrency, deadline, and
//!   progress callback.
//!
//! Optional analysis:
//!
//! - [`correlate`] — group accounts that look like the same person
//!   across sites via [`enriched`](crate::correlate::correlate)
//!   profile fields.
//! - [`permute`] — generate username variants
//!   (alice → alice1, alice.dev, …) via [`MAX_VARIANTS`] /
//!   [`PermuteLevel`].
//! - [`doctor`] — registry health check
//!   ([`check_site`](crate::doctor::check_site)), signature
//!   derivation ([`suggest_fix`](crate::doctor::suggest_fix)),
//!   known-present discovery
//!   ([`discover_known_present`](crate::doctor::discover_known_present)),
//!   site scaffolding ([`scaffold_site`](crate::doctor::scaffold_site)).
//!
//! Bot-protected sites (Instagram, X/Twitter today):
//!
//! - [`BrowserBackend`] trait — abstract real-Chrome driver.
//!   Configurable on the [`Client`] via
//!   [`ClientBuilder::browser`](ClientBuilder::browser). Built-in
//!   implementations: [`browser::local::LocalBackend`] (free, via
//!   `chromiumoxide`) and
//!   [`browser::browserbase::BrowserbaseBackend`] (cloud, residential
//!   IPs, in-tree raw async CDP client). [`BrowserBudget`] caps
//!   browser-routed fetches per scan to keep cost predictable.
//!
//! ## Cache
//!
//! [`Cache`] persists per-(site, username, signal-signature) verdicts
//! between runs. Compose with [`Client`] via the builder or skip
//! entirely for one-shot scans.
//!
//! ## Error model
//!
//! [`Result`] is a `Result<T, Error>` alias; [`Error`] is a single
//! crate-level `thiserror` enum. The probe path *never* surfaces
//! errors — transient network failures become
//! [`MatchKind::Uncertain`] with a typed [`UncertainReason`], so
//! you get a partial result for every site even when the network is
//! flaky. Loader errors (malformed registry JSON, invalid CSS
//! selectors, regex compile failures) come back as `Err`.
//!
//! ## Version history
//!
//! Pre-1.0 `SemVer`. Breaking changes since 0.1:
//!
//! - **0.2.0** — added [`Site::request_headers`] (`BTreeMap<String,
//!   String>`); [`BrowserBackend::fetch`] gained the `headers`
//!   parameter; [`browser`] module became `pub`.
//! - **0.3.0** — [`Site::known_present`] changed from
//!   `Option<String>` to `Option<KnownPresent>` (the new enum
//!   accepts string-or-array via untagged serde);
//!   [`DoctorReport::Healthy::present`] and
//!   `Unhealthy::present` changed from `Option<CheckOutcome>` to
//!   `Vec<(String, CheckOutcome)>` (one entry per probed candidate).
//! - **0.4.0** — [`Registry::filter`] gained a fifth
//!   `include_nsfw: bool` parameter (default-exclude adult sites);
//!   [`UncertainReason`] gained `UsernameNotAllowed`;
//!   [`Site::regex_check`] field added (per-site username regex).
//!
//! Each change has a migration block in [the
//! CHANGELOG](https://github.com/commit3296/adler/blob/main/CHANGELOG.md).

mod access;
mod ban;
mod cache;
mod check;
mod client;
mod confidence;
mod correlate;
pub mod doctor;
mod enrich;
mod error;
mod escalation;
pub mod executor;
mod permute;
mod profile;
mod registry;
mod retry;
mod robots;
mod site;
pub mod telemetry;
#[cfg(test)]
mod test_fixtures;
mod throttle;
mod transport;
mod username;

pub mod browser;

pub use access::{
    AccessPolicy, CountryCode, EgressKind, EgressSpec, EgressSummary, Session, SessionStore,
};
pub use browser::{BrowserBackend, BrowserBudget, RenderedPage};
pub use cache::Cache;
pub use check::{CheckOutcome, MatchKind, UncertainReason};
pub use client::{
    BOT_PROTECTED_TAG, Client, ClientBuilder, DEFAULT_BROWSER_BUDGET, DEFAULT_ESCALATION_BUDGET,
    RawResponse,
};
pub use confidence::{ConfidenceLabel, ConfidenceReason, ConfidenceScore};
pub use correlate::{Cluster, CorrelationReport, LINK_THRESHOLD, correlate};
pub use doctor::{DoctorReport, ExtractSuggestion, FixSuggestion};
pub use error::{Error, Result};
pub use escalation::{EscalationBudget, TransportTier};
pub use executor::ExecutorOptions;
pub use permute::{MAX_VARIANTS, PermuteLevel, permute};
pub use profile::{EvidenceOrigin, EvidenceSource, ProfileEvidence, ProfileEvidenceKind};
pub use registry::{Registry, SiteFilter};
pub use site::{
    Engine, Extractor, HttpMethod, KnownPresent, ProtectionKind, Signal, Site, UrlTemplate,
};
pub use username::Username;
