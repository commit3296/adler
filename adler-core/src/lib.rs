//! Core engine for the Adler OSINT username-search tool.
//!
//! The crate is the runtime-agnostic library half of Adler; the binary
//! interface lives in `adler-cli`. Key items:
//!
//! - [`Username`] — validated search target
//! - [`Site`], [`Signal`], [`UrlTemplate`] — site registry types
//! - [`Registry`] — load and filter the embedded or a custom site list
//! - [`Client`], [`ClientBuilder`] — `reqwest`-backed probe issuer
//! - [`executor::run`] — bounded-concurrency fan-out runner
//! - [`CheckOutcome`], [`MatchKind`] — verdict types
//! - [`correlate`], [`permute`], [`doctor`] — enrichment-driven analysis
//!
//! # Example
//!
//! Scan the embedded registry for a username and print the hits:
//!
//! ```no_run
//! use adler_core::{Client, ExecutorOptions, MatchKind, Registry, Username, executor};
//!
//! # async fn run() -> adler_core::Result<()> {
//! let registry = Registry::default_embedded()?;
//! // filter(include, exclude, tags, exclude_tags) — empty slices = no filter.
//! let sites = registry.filter(&["github".into()], &[], &[], &[]);
//! let username = Username::new("torvalds")?;
//! let client = Client::builder().build()?;
//!
//! let outcomes = executor::run(&client, &sites, &username, ExecutorOptions::default()).await;
//! for outcome in outcomes.iter().filter(|o| o.kind == MatchKind::Found) {
//!     println!("found: {} → {}", outcome.site, outcome.url);
//! }
//! # Ok(())
//! # }
//! ```

mod ban;
mod cache;
mod check;
mod client;
mod correlate;
pub mod doctor;
mod enrich;
mod error;
pub mod executor;
mod permute;
mod registry;
mod retry;
mod robots;
mod site;
mod throttle;
mod username;

pub use cache::Cache;
pub use check::{CheckOutcome, MatchKind, UncertainReason};
pub use client::{Client, ClientBuilder, RawResponse};
pub use correlate::{Cluster, CorrelationReport, LINK_THRESHOLD, correlate};
pub use doctor::{DoctorReport, FixSuggestion};
pub use error::{Error, Result};
pub use executor::ExecutorOptions;
pub use permute::{MAX_VARIANTS, PermuteLevel, permute};
pub use registry::Registry;
pub use site::{Extractor, Signal, Site, UrlTemplate};
pub use username::Username;
