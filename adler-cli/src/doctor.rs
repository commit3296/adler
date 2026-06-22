//! `--doctor` mode and its three `--apply` families.
//!
//! Owns the registry-health probe (`run_doctor`), the `--fix`,
//! `--suggest-known-present`, and `--suggest-extract` suggestion
//! drivers (unified through [`DoctorSuggestionApplier`]), the atomic
//! `sites.json` patcher ([`patch_registry_field`]), and the
//! `--suggest-protection` reader over persisted scan history.
//!
//! Lives in its own module so `main.rs` stays focused on CLI parsing
//! and top-level dispatch — see CLAUDE.md for the broader splitting
//! rationale.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use adler_core::{
    BOT_PROTECTED_TAG, CheckOutcome, Client, DoctorReport, MatchKind, ProtectionKind, Site,
    TransportTier, Username, doctor,
};
use anyhow::{Context as _, Result};
use serde::Serialize;

use crate::OutputFormat;

// Internal CLI options struct — the variants are orthogonal independent
// toggles, not a state machine. The pedantic lint doesn't apply.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct DoctorOpts<'a> {
    pub(crate) fix: bool,
    pub(crate) apply: bool,
    pub(crate) yes: bool,
    pub(crate) suggest_known_present: bool,
    pub(crate) suggest_extract: bool,
    pub(crate) suggest_protection: bool,
    pub(crate) browser_matrix: bool,
    pub(crate) sites_path: Option<&'a Path>,
    pub(crate) scans_dir: Option<&'a Path>,
    pub(crate) color: bool,
    pub(crate) format: OutputFormat,
}

impl DoctorOpts<'_> {
    fn suggest_protection_only(&self) -> bool {
        self.suggest_protection
            && !self.fix
            && !self.apply
            && !self.suggest_known_present
            && !self.suggest_extract
            && !self.browser_matrix
    }
}

pub(crate) async fn run_doctor(
    client: &Client,
    sites: &[Site],
    opts: DoctorOpts<'_>,
) -> Result<ExitCode> {
    // The suggest/apply helper paths still emit human text; the structured
    // formats target the headline walk. csv/html aren't a natural fit for
    // doctor output — surface that rather than letting --format silently
    // no-op.
    match opts.format {
        OutputFormat::Text | OutputFormat::Json | OutputFormat::Ndjson => {}
        OutputFormat::Csv | OutputFormat::Html => {
            anyhow::bail!(
                "--doctor supports --format text|json|ndjson (got {:?})",
                opts.format
            );
        }
    }
    if opts.suggest_protection_only() {
        // `--suggest-protection` is telemetry-only: it reads persisted scan
        // history and must not launch a live registry health walk.
        tracing::info!(scans_dir = ?opts.scans_dir, "starting protection telemetry suggestions");
        print_protection_suggestions(opts.scans_dir);
        return Ok(ExitCode::SUCCESS);
    }

    tracing::info!(
        count = sites.len(),
        fix = opts.fix,
        apply = opts.apply,
        suggest_known_present = opts.suggest_known_present,
        suggest_extract = opts.suggest_extract,
        suggest_protection = opts.suggest_protection,
        browser_matrix = opts.browser_matrix,
        format = ?opts.format,
        "starting doctor"
    );

    let walk = walk_doctor_sites(client, sites, opts.format, opts.color).await?;
    let browser_matrix = if opts.browser_matrix {
        collect_browser_matrix(client, sites).await?
    } else {
        Vec::new()
    };
    render_browser_matrix(opts.format, opts.browser_matrix, &browser_matrix)?;
    render_doctor_summary(
        opts.format,
        sites.len(),
        &walk,
        opts.browser_matrix,
        &browser_matrix,
    )?;
    run_doctor_suggestions(client, &opts, &walk).await?;

    // `--apply` is meaningless without something to apply: clap allows
    // `--apply --sites <path>` on its own (since `--fix` /
    // `--suggest-known-present` / `--suggest-extract` are siblings) but
    // a bare combination is just a typo. Surface it rather than
    // silently doing nothing.
    if opts.apply && !opts.fix && !opts.suggest_known_present && !opts.suggest_extract {
        anyhow::bail!(
            "--apply requires --fix, --suggest-known-present, and/or --suggest-extract \
             to know what to patch"
        );
    }

    if opts.suggest_protection {
        // Scope-independent of the site-health check above: this draws
        // on persisted scan history, not on a live registry probe.
        print_protection_suggestions(opts.scans_dir);
    }

    Ok(if walk.failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Buckets accumulated by [`walk_doctor_sites`] — the headline walk's
/// state, owned outside `run_doctor` so output rendering and suggestion
/// dispatch can each consume the slices they care about.
struct DoctorWalk<'a> {
    failures: usize,
    failed_sites: Vec<&'a Site>,
    healthy_sites: Vec<&'a Site>,
    /// Per-site records, only populated when format == Json (Ndjson
    /// streams them eagerly inside the loop).
    records: Vec<serde_json::Value>,
}

/// Probe every site once, print the per-site line for text output (or
/// stream the ndjson record), and return the failed/healthy partition
/// plus the buffered JSON records for the final envelope.
async fn walk_doctor_sites<'a>(
    client: &Client,
    sites: &'a [Site],
    format: OutputFormat,
    color: bool,
) -> Result<DoctorWalk<'a>> {
    let structured = matches!(format, OutputFormat::Json | OutputFormat::Ndjson);
    let mut walk = DoctorWalk {
        failures: 0,
        failed_sites: Vec::new(),
        healthy_sites: Vec::new(),
        records: if matches!(format, OutputFormat::Json) {
            Vec::with_capacity(sites.len())
        } else {
            Vec::new()
        },
    };
    for site in sites {
        let report = doctor::check_site(client, site).await;
        let (verdict, issues): (&'static str, Vec<String>) = match &report {
            DoctorReport::Healthy { .. } => ("healthy", Vec::new()),
            DoctorReport::Unhealthy { issues, .. } => ("unhealthy", issues.clone()),
        };
        match report {
            DoctorReport::Healthy { .. } => walk.healthy_sites.push(site),
            DoctorReport::Unhealthy { .. } => {
                walk.failures += 1;
                walk.failed_sites.push(site);
            }
        }
        if structured {
            let record = serde_json::json!({
                "name": site.name,
                "verdict": verdict,
                "issues": issues,
            });
            match format {
                OutputFormat::Ndjson => println!(
                    "{}",
                    serde_json::to_string(&record)
                        .context("serialising doctor site record as ndjson")?
                ),
                OutputFormat::Json => walk.records.push(record),
                _ => unreachable!("structured implies json/ndjson"),
            }
        } else if verdict == "healthy" {
            if color {
                println!("\x1b[32m[OK]\x1b[0m   {}", site.name);
            } else {
                println!("[OK]   {}", site.name);
            }
        } else {
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
    Ok(walk)
}

/// Emit the trailing summary line (text), tagged summary record
/// (ndjson) or the full `{sites, summary}` envelope (json).
fn render_doctor_summary(
    format: OutputFormat,
    total: usize,
    walk: &DoctorWalk<'_>,
    browser_matrix_enabled: bool,
    browser_matrix: &[BrowserMatrixRow],
) -> Result<()> {
    let summary = serde_json::json!({
        "total": total,
        "healthy": walk.healthy_sites.len(),
        "failing": walk.failures,
    });
    match format {
        OutputFormat::Text => {
            println!();
            println!("{total} site(s) checked, {} failed", walk.failures);
        }
        OutputFormat::Ndjson => {
            // Tagged differently from per-site records so consumers can
            // distinguish without positional logic.
            let summary_line = serde_json::json!({
                "type": "summary",
                "total": total,
                "healthy": walk.healthy_sites.len(),
                "failing": walk.failures,
            });
            println!(
                "{}",
                serde_json::to_string(&summary_line)
                    .context("serialising doctor summary as ndjson")?
            );
        }
        OutputFormat::Json => {
            let mut envelope = serde_json::json!({
                "sites": walk.records,
                "summary": summary,
            });
            if browser_matrix_enabled {
                envelope["browser_matrix"] = serde_json::to_value(browser_matrix)
                    .context("serialising browser matrix for doctor json")?;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&envelope)
                    .context("serialising doctor report as json")?
            );
        }
        OutputFormat::Csv | OutputFormat::Html => unreachable!("rejected at function entry"),
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
struct BrowserMatrixRow {
    site: String,
    username: String,
    raw: BrowserMatrixOutcome,
    configured: BrowserMatrixOutcome,
}

#[derive(Debug, Clone, Serialize)]
struct BrowserMatrixOutcome {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<adler_core::UncertainReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<TransportTier>,
    #[serde(skip_serializing_if = "is_zero_u8")]
    escalations: u8,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_u8(value: &u8) -> bool {
    *value == 0
}

impl BrowserMatrixOutcome {
    fn from_outcome(outcome: &CheckOutcome) -> Self {
        Self {
            kind: match_kind_label(outcome.kind),
            reason: outcome.reason.clone(),
            transport: outcome.transport,
            escalations: outcome.escalations,
        }
    }
}

async fn collect_browser_matrix(client: &Client, sites: &[Site]) -> Result<Vec<BrowserMatrixRow>> {
    let raw_client = client.without_browser_for_diagnostics();
    let configured_client = client.with_fresh_budgets_for_diagnostics();
    let mut rows = Vec::new();

    for site in sites
        .iter()
        .filter(|site| routes_through_browser_like(site))
    {
        let Some(username) = site.known_present.as_ref().and_then(|kp| kp.primary()) else {
            continue;
        };
        let user = match Username::new(username.to_owned()) {
            Ok(user) => user,
            Err(err) => {
                tracing::warn!(
                    site = %site.name,
                    username,
                    error = %err,
                    "skipping invalid known_present in browser matrix"
                );
                continue;
            }
        };
        let raw = raw_client.check(site, &user).await;
        let configured = configured_client.check(site, &user).await;
        rows.push(BrowserMatrixRow {
            site: site.name.clone(),
            username: username.to_owned(),
            raw: BrowserMatrixOutcome::from_outcome(&raw),
            configured: BrowserMatrixOutcome::from_outcome(&configured),
        });
    }

    Ok(rows)
}

fn render_browser_matrix(
    format: OutputFormat,
    browser_matrix_enabled: bool,
    rows: &[BrowserMatrixRow],
) -> Result<()> {
    if !browser_matrix_enabled {
        return Ok(());
    }
    match format {
        OutputFormat::Text => {
            println!();
            println!("Browser matrix (protected known-present sites):");
            if rows.is_empty() {
                println!("  no protected sites with known_present were eligible");
                return Ok(());
            }
            for row in rows {
                println!(
                    "  {} / {}: raw={} configured={}",
                    row.site,
                    row.username,
                    matrix_outcome_text(&row.raw),
                    matrix_outcome_text(&row.configured)
                );
            }
        }
        OutputFormat::Ndjson => {
            for row in rows {
                let record = serde_json::json!({
                    "type": "browser_matrix",
                    "site": &row.site,
                    "username": &row.username,
                    "raw": &row.raw,
                    "configured": &row.configured,
                });
                println!(
                    "{}",
                    serde_json::to_string(&record)
                        .context("serialising browser matrix record as ndjson")?
                );
            }
        }
        OutputFormat::Json => {}
        OutputFormat::Csv | OutputFormat::Html => unreachable!("rejected at function entry"),
    }
    Ok(())
}

fn matrix_outcome_text(outcome: &BrowserMatrixOutcome) -> String {
    let mut text = outcome.kind.to_owned();
    if let Some(reason) = &outcome.reason {
        text.push('(');
        text.push_str(&reason.to_string());
        text.push(')');
    }
    if let Some(transport) = outcome.transport {
        text.push_str(" via ");
        text.push_str(transport.as_str());
    }
    if outcome.escalations > 0 {
        text.push_str(" escalations=");
        text.push_str(&outcome.escalations.to_string());
    }
    text
}

fn match_kind_label(kind: MatchKind) -> &'static str {
    match kind {
        MatchKind::Found => "found",
        MatchKind::NotFound => "not_found",
        MatchKind::Uncertain => "uncertain",
    }
}

fn routes_through_browser_like(site: &Site) -> bool {
    site.tags
        .iter()
        .any(|tag| tag.eq_ignore_ascii_case(BOT_PROTECTED_TAG))
        || site
            .protection
            .iter()
            .any(|kind| !matches!(kind, ProtectionKind::UserAuth))
}

/// Dispatch the three suggestion modes (`--fix`,
/// `--suggest-known-present`, `--suggest-extract`); each is gated on
/// its CLI flag and on having an input population to act on.
async fn run_doctor_suggestions(
    client: &Client,
    opts: &DoctorOpts<'_>,
    walk: &DoctorWalk<'_>,
) -> Result<()> {
    if opts.fix && !walk.failed_sites.is_empty() {
        if opts.apply {
            // `--apply` requires `--sites` (enforced by clap), so this is
            // always Some by construction; the `?` is just belt-and-braces.
            let path = opts
                .sites_path
                .context("internal: --apply reached run_doctor without --sites")?;
            apply_fix_suggestions(client, &walk.failed_sites, path, opts.yes).await?;
        } else {
            print_fix_suggestions(client, &walk.failed_sites).await?;
        }
    }

    if opts.suggest_known_present && !walk.failed_sites.is_empty() {
        if opts.apply {
            let path = opts.sites_path.context(
                "internal: --apply --suggest-known-present reached run_doctor without --sites",
            )?;
            apply_known_present_suggestions(client, &walk.failed_sites, path, opts.yes).await?;
        } else {
            print_known_present_suggestions(client, &walk.failed_sites).await?;
        }
    }

    if opts.suggest_extract {
        // Extractor derivation only makes sense on sites whose
        // known_present user actually resolves — that's exactly the
        // `Healthy` population the walk above identified. Sites that
        // already declare `extract` rules are skipped so hand-authored
        // selectors aren't clobbered.
        let candidates: Vec<&Site> = walk
            .healthy_sites
            .iter()
            .copied()
            .filter(|s| s.extract.is_empty())
            .collect();
        if !candidates.is_empty() {
            if opts.apply {
                let path = opts.sites_path.context(
                    "internal: --apply --suggest-extract reached run_doctor without --sites",
                )?;
                apply_extract_suggestions(client, &candidates, path, opts.yes).await?;
            } else {
                print_extract_suggestions(client, &candidates).await?;
            }
        }
    }
    Ok(())
}

/// One of the three doctor suggestion modes — `--fix`,
/// `--suggest-known-present`, `--suggest-extract`. The trait
/// captures every per-mode variant point so [`apply_suggestions`]
/// can drive the shared loop generically.
trait DoctorSuggestionApplier {
    /// Concrete patch type — `Vec<Signal>` for fix,
    /// `String` for `known_present`, `Vec<Extractor>` for extract.
    type Patch;

    /// Run the per-site discovery; return `Some((patch, rationale))`
    /// when the site has an actionable suggestion, `None` when it
    /// should be skipped. The returned future must be `Send`
    /// because the shared loop awaits it inside a multi-threaded
    /// tokio runtime.
    fn discover(
        &self,
        client: &Client,
        site: &Site,
    ) -> impl std::future::Future<Output = Option<(Self::Patch, String)>> + Send;

    /// Intro line: `"\ngathering fix suggestions for N failing site(s)…"`.
    fn gather_header(&self, n: usize) -> String;

    /// One-line skip message for a site that produced no suggestion.
    fn skip_message(&self, site: &Site) -> String;

    /// Wording for "nothing to write" when no suggestion landed.
    fn empty_message(&self) -> &'static str;

    /// Header above the proposed-changes diff block.
    fn proposed_header(&self) -> &'static str;

    /// Render one site's diff entry. The shared loop passes the
    /// in-memory `Site` (when present) so impls can read original
    /// values for an old → new diff.
    fn render_diff(
        &self,
        original: Option<&Site>,
        name: &str,
        patch: &Self::Patch,
        rationale: &str,
    );

    /// Field name to write in `sites.json`. Passed to
    /// [`patch_registry_field`].
    fn field_name(&self) -> &'static str;
}

/// Drive the shared apply loop: gather → diff → confirm → patch.
/// Every `--apply` family path delegates here; only the variant
/// methods on the [`DoctorSuggestionApplier`] differ.
async fn apply_suggestions<A>(
    applier: &A,
    client: &Client,
    sites: &[&Site],
    sites_path: &Path,
    skip_prompt: bool,
) -> Result<()>
where
    A: DoctorSuggestionApplier + Sync,
    A::Patch: serde::Serialize + Send,
{
    println!("{}", applier.gather_header(sites.len()));
    let mut gathered: Vec<(String, A::Patch, String)> = Vec::new();
    for site in sites {
        if let Some((patch, rationale)) = applier.discover(client, site).await {
            gathered.push((site.name.clone(), patch, rationale));
        } else {
            println!("{}", applier.skip_message(site));
        }
    }
    if gathered.is_empty() {
        println!("\n{}", applier.empty_message());
        return Ok(());
    }

    let in_memory: std::collections::HashMap<&str, &Site> =
        sites.iter().map(|s| (s.name.as_str(), *s)).collect();
    println!("\n{}", applier.proposed_header());
    for (name, patch, rationale) in &gathered {
        applier.render_diff(
            in_memory.get(name.as_str()).copied(),
            name,
            patch,
            rationale,
        );
    }
    println!(
        "\n{} site(s) to patch in {}",
        gathered.len(),
        sites_path.display()
    );

    if !confirm_apply(skip_prompt)? {
        return Ok(());
    }

    let patches: Vec<(String, A::Patch)> = gathered.into_iter().map(|(n, p, _)| (n, p)).collect();
    let report = patch_registry_field(sites_path, &patches, applier.field_name())?;

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

/// `--fix` implementation of [`DoctorSuggestionApplier`].
struct FixApplier;

impl DoctorSuggestionApplier for FixApplier {
    type Patch = Vec<adler_core::Signal>;

    async fn discover(&self, client: &Client, site: &Site) -> Option<(Self::Patch, String)> {
        doctor::suggest_fix(client, site)
            .await
            .map(|f| (f.signals, f.rationale))
    }

    fn gather_header(&self, n: usize) -> String {
        format!("\ngathering fix suggestions for {n} failing site(s)…")
    }

    fn skip_message(&self, site: &Site) -> String {
        format!(
            "  {}  — skipped (no suggestion; responses indistinguishable)",
            site.name
        )
    }

    fn empty_message(&self) -> &'static str {
        "no applicable fixes — nothing to write."
    }

    fn proposed_header(&self) -> &'static str {
        "proposed changes:"
    }

    fn render_diff(
        &self,
        original: Option<&Site>,
        name: &str,
        patch: &Self::Patch,
        rationale: &str,
    ) {
        println!("\n  {name}  ({rationale})");
        if let Some(site) = original {
            for old in &site.signals {
                println!("    - {}", render_signal(old));
            }
        }
        for new in patch {
            println!("    + {}", render_signal(new));
        }
    }

    fn field_name(&self) -> &'static str {
        "signals"
    }
}

/// `--suggest-known-present` implementation of
/// [`DoctorSuggestionApplier`].
struct KnownPresentApplier;

impl DoctorSuggestionApplier for KnownPresentApplier {
    type Patch = String;

    async fn discover(&self, client: &Client, site: &Site) -> Option<(Self::Patch, String)> {
        let pool = doctor::default_candidate_pool(site);
        let pool_len = pool.len();
        doctor::discover_known_present(client, site, &pool)
            .await
            .map(|candidate| (candidate, format!("matched in pool of {pool_len}")))
    }

    fn gather_header(&self, n: usize) -> String {
        format!("\ndiscovering known_present candidates for {n} failing site(s)…")
    }

    fn skip_message(&self, site: &Site) -> String {
        let pool_len = doctor::default_candidate_pool(site).len();
        format!(
            "  {}  — skipped (no candidate matched in pool of {pool_len})",
            site.name
        )
    }

    fn empty_message(&self) -> &'static str {
        "no applicable patches — nothing to write."
    }

    fn proposed_header(&self) -> &'static str {
        "proposed known_present changes:"
    }

    fn render_diff(
        &self,
        original: Option<&Site>,
        name: &str,
        patch: &Self::Patch,
        _rationale: &str,
    ) {
        let old = original
            .and_then(|s| s.known_present.as_ref())
            .and_then(adler_core::KnownPresent::primary)
            .unwrap_or("<none>");
        println!("  {name}");
        println!("    - {old:?}");
        println!("    + {patch:?}");
    }

    fn field_name(&self) -> &'static str {
        "known_present"
    }
}

/// `--suggest-extract` implementation of [`DoctorSuggestionApplier`].
struct ExtractApplier;

impl DoctorSuggestionApplier for ExtractApplier {
    type Patch = Vec<adler_core::Extractor>;

    async fn discover(&self, client: &Client, site: &Site) -> Option<(Self::Patch, String)> {
        doctor::suggest_extract(client, site)
            .await
            .map(|s| (s.extractors, s.rationale))
    }

    fn gather_header(&self, n: usize) -> String {
        format!("\nderiving extract blocks for {n} candidate site(s)…")
    }

    fn skip_message(&self, site: &Site) -> String {
        format!(
            "  {}  — skipped (page exposed no OpenGraph or Twitter Card metadata)",
            site.name
        )
    }

    fn empty_message(&self) -> &'static str {
        "no applicable patches — nothing to write."
    }

    fn proposed_header(&self) -> &'static str {
        "proposed extract blocks:"
    }

    fn render_diff(
        &self,
        _original: Option<&Site>,
        name: &str,
        patch: &Self::Patch,
        rationale: &str,
    ) {
        println!("  {name}  ({rationale})");
        for e in patch {
            let attr = e.attr.as_deref().unwrap_or("<text>");
            println!(
                "    + {field}: {selector} [{attr}]",
                field = e.field,
                selector = e.selector,
            );
        }
    }

    fn field_name(&self) -> &'static str {
        "extract"
    }
}

/// Common interactive confirmation for every `--apply` family path.
/// Returns `Ok(true)` when the user accepts (or `skip` is set),
/// `Ok(false)` after printing the "aborted" message so the caller
/// can return cleanly.
fn confirm_apply(skip: bool) -> Result<bool> {
    if skip {
        return Ok(true);
    }
    print!("Apply? [y/N] ");
    io::stdout().flush().ok();
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .context("reading confirmation prompt")?;
    if matches!(answer.trim(), "y" | "Y" | "yes" | "YES") {
        Ok(true)
    } else {
        println!("aborted; no changes written.");
        Ok(false)
    }
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

/// `--fix --apply`: gather diffed signal suggestions, confirm, patch.
async fn apply_fix_suggestions(
    client: &Client,
    failed: &[&Site],
    sites_path: &Path,
    skip_prompt: bool,
) -> Result<()> {
    apply_suggestions(&FixApplier, client, failed, sites_path, skip_prompt).await
}

#[derive(Debug, Default, PartialEq, Eq)]
struct PatchReport {
    /// How many site entries were updated.
    patched: usize,
    /// Names that had a suggestion but no matching JSON entry — skipped,
    /// not erased.
    missing: Vec<String>,
}

/// Pure helper: load the JSON registry file, walk the `sites` array,
/// replace one named field on each entry matched by name, and write
/// back atomically through a sibling `*.tmp`. The three concrete
/// `patch_*_in_sites_file` helpers are thin shims around this.
///
/// `T` only needs `serde::Serialize` — the value is materialised via
/// `serde_json::to_value` so each apply path can pass the natural
/// Rust type (a `Vec<Signal>`, a `String`, a `Vec<Extractor>`)
/// without an intermediate conversion.
fn patch_registry_field<T: serde::Serialize>(
    sites_path: &Path,
    patches: &[(String, T)],
    field: &str,
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
    for (name, value) in patches {
        let entry = arr.iter_mut().find_map(|v| {
            let obj = v.as_object_mut()?;
            (obj.get("name").and_then(serde_json::Value::as_str) == Some(name.as_str()))
                .then_some(obj)
        });
        match entry {
            Some(obj) => {
                let json = serde_json::to_value(value)
                    .with_context(|| format!("serialising {field} value for {name:?}"))?;
                obj.insert(field.into(), json);
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

/// Render an [`adler_core::Signal`] in compact JSON for the diff output.
/// Falls back to the `Debug` impl on the (impossible) serialisation
/// failure so the diff always has something to show.
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

/// `--suggest-known-present --apply`: probe candidate users for
/// each failing site, render old → new diff, confirm, patch.
async fn apply_known_present_suggestions(
    client: &Client,
    failed: &[&Site],
    sites_path: &Path,
    skip_prompt: bool,
) -> Result<()> {
    apply_suggestions(
        &KnownPresentApplier,
        client,
        failed,
        sites_path,
        skip_prompt,
    )
    .await
}

/// `--suggest-extract` dry-run path: for each healthy candidate site,
/// fetch the `known_present` profile page, derive an `extract` block
/// from its `OpenGraph` / Twitter Card metadata, and print a paste-ready
/// JSON snippet. Nothing is modified.
async fn print_extract_suggestions(client: &Client, candidates: &[&Site]) -> Result<()> {
    println!("\nsuggested extract blocks (review before applying — paste into a --sites file):\n");
    let mut suggested = 0_usize;
    for site in candidates {
        match doctor::suggest_extract(client, site).await {
            Some(suggestion) => {
                suggested += 1;
                let extractors = serde_json::to_string(&suggestion.extractors)
                    .unwrap_or_else(|_| "[]".to_owned());
                println!("  {}  ({})", suggestion.site, suggestion.rationale);
                println!(
                    "    {{\"name\": {:?}, \"extract\": {}}}",
                    site.name, extractors,
                );
            }
            None => {
                println!(
                    "  {}  — no suggestion (page exposed no OpenGraph or Twitter Card metadata)",
                    site.name
                );
            }
        }
    }
    println!(
        "\n{suggested} of {} candidate site(s) produced a suggestion",
        candidates.len(),
    );
    Ok(())
}

/// `--suggest-extract --apply`: derive OpenGraph/Twitter-Card-based
/// extract blocks for healthy sites without one, confirm, patch.
async fn apply_extract_suggestions(
    client: &Client,
    candidates: &[&Site],
    sites_path: &Path,
    skip_prompt: bool,
) -> Result<()> {
    apply_suggestions(&ExtractApplier, client, candidates, sites_path, skip_prompt).await
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

#[cfg(test)]
mod tests {
    use super::*;

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

        let report = patch_registry_field(&path, &patches, "signals").expect("patch ok");
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
        let err = patch_registry_field(&path, &patches, "signals").unwrap_err();
        assert!(
            err.to_string().contains("no top-level \"sites\" array"),
            "unexpected error: {err}",
        );
    }

    #[test]
    fn patch_known_present_replaces_and_preserves_other_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sites.json");
        std::fs::write(
            &path,
            r#"{
  "engines": {},
  "sites": [
    { "name": "Stale", "url": "https://stale.example/{username}",
      "known_present": "blue",
      "signals": [{"kind": "status_found", "codes": [200]}],
      "tags": ["dev"] },
    { "name": "Untouched", "url": "https://other.example/{username}",
      "known_present": "torvalds",
      "signals": [{"kind": "status_found", "codes": [200]}] }
  ]
}"#,
        )
        .unwrap();

        let patches = vec![
            ("Stale".to_owned(), "alice".to_owned()),
            ("Untouched".to_owned(), "octocat".to_owned()),
            ("Never-existed".to_owned(), "ghost".to_owned()),
        ];
        let report = patch_registry_field(&path, &patches, "known_present").expect("ok");
        assert_eq!(report.patched, 2);
        assert_eq!(report.missing, vec!["Never-existed".to_owned()]);

        let written = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&written).unwrap();
        let arr = v["sites"].as_array().unwrap();
        let stale = arr.iter().find(|s| s["name"] == "Stale").unwrap();
        assert_eq!(stale["known_present"], "alice");
        // Other fields preserved untouched.
        assert_eq!(stale["url"], "https://stale.example/{username}");
        assert_eq!(stale["tags"], serde_json::json!(["dev"]));
        assert!(stale["signals"].is_array());

        let untouched = arr.iter().find(|s| s["name"] == "Untouched").unwrap();
        assert_eq!(untouched["known_present"], "octocat");

        // No leftover *.tmp.
        let tmp_path = path.with_extension("json.tmp");
        assert!(!tmp_path.exists());
    }

    #[test]
    fn patch_extract_writes_block_and_preserves_other_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sites.json");
        std::fs::write(
            &path,
            r#"{
  "engines": {},
  "sites": [
    { "name": "Bare", "url": "https://bare.example/{username}",
      "known_present": "alice",
      "signals": [{"kind": "status_found", "codes": [200]}],
      "tags": ["dev"] },
    { "name": "Untouched", "url": "https://other.example/{username}",
      "known_present": "torvalds",
      "signals": [{"kind": "status_found", "codes": [200]}] }
  ]
}"#,
        )
        .unwrap();

        let block = vec![
            adler_core::Extractor {
                field: "name".into(),
                selector: r#"meta[property="og:title"]"#.into(),
                attr: Some("content".into()),
            },
            adler_core::Extractor {
                field: "avatar".into(),
                selector: r#"meta[property="og:image"]"#.into(),
                attr: Some("content".into()),
            },
        ];
        let patches = vec![
            ("Bare".to_owned(), block.clone()),
            ("Never-existed".to_owned(), block),
        ];
        let report = patch_registry_field(&path, &patches, "extract").expect("ok");
        assert_eq!(report.patched, 1);
        assert_eq!(report.missing, vec!["Never-existed".to_owned()]);

        let written = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&written).unwrap();
        let arr = v["sites"].as_array().unwrap();
        let bare = arr.iter().find(|s| s["name"] == "Bare").unwrap();
        let extract = bare["extract"].as_array().unwrap();
        assert_eq!(extract.len(), 2);
        assert_eq!(extract[0]["field"], "name");
        assert_eq!(extract[0]["selector"], r#"meta[property="og:title"]"#);
        assert_eq!(extract[0]["attr"], "content");
        assert_eq!(extract[1]["field"], "avatar");
        // Other fields preserved untouched.
        assert_eq!(bare["url"], "https://bare.example/{username}");
        assert_eq!(bare["known_present"], "alice");
        assert_eq!(bare["tags"], serde_json::json!(["dev"]));
        // Sibling entry without a patch must come through unchanged.
        let untouched = arr.iter().find(|s| s["name"] == "Untouched").unwrap();
        assert!(untouched.get("extract").is_none());
        assert_eq!(untouched["known_present"], "torvalds");

        // No leftover *.tmp.
        let tmp_path = path.with_extension("json.tmp");
        assert!(!tmp_path.exists());
    }
}
