//! Scan-mode drivers: single-username, batch, and watch.
//!
//! Owns the entry points clap dispatches to when the user runs
//! `adler <username>` (`run_scan`), `--input file.txt` (`run_batch`),
//! or `--watch` (`run_watch`), plus the shared scaffolding (executor
//! options, cache load/save, permutation fan-out, audit log,
//! since-last-scan diff). Everything network- and I/O-bound lives
//! here; pure formatting is in `output`.

use std::io::{self, IsTerminal as _, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use adler_core::{
    AvatarHashOptions, Cache, CheckOutcome, Client, EvidenceAccessPath, ExecutorOptions, MatchKind,
    ProfileEvidence, ProfileEvidenceKind, Site, Username, correlate, executor, fetch_avatar_hash,
    permute,
};
use anyhow::{Context as _, Result};
use futures::{StreamExt as _, stream};

use crate::output::{
    CSV_COLUMNS, DisplayOpts, OutputOpts, any_found, make_progress_bar, outcome_csv_fields,
    print_correlation, print_hint, print_row, print_tally, should_show, stream_row, write_csv_row,
    write_outputs,
};
use crate::transport::TOR_PROXY;
use crate::{Cli, OutputFormat, cache_path};

const AVATAR_HASH_CONCURRENCY: usize = 4;

/// Drive a username scan (with permutation variants), then emit results.
///
/// Split out of `run` so the dispatcher stays small; the network/I/O parts
/// live here while the pure pieces it relies on (`write_outputs`,
/// `any_found`) are unit-tested directly.
pub(crate) async fn run_scan(cli: &Cli, client: &Client, sites: &[Site]) -> Result<ExitCode> {
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
    // it), not per variant. --enrich / --correlate / --avatar-hash want
    // fresh data, so they bypass it. Each variant is a distinct username key
    // within the cache.
    let use_cache = !cli.no_cache && !cli.enrich && !cli.correlate && !cli.avatar_hash;
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
    let live = matches!(cli.format, OutputFormat::Text) && stdout_tty && !cli.avatar_hash;

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
            print_hint(
                &mut out,
                cli.enrich || cli.avatar_hash,
                cli.correlate,
                display.color,
            )?;
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
    attach_avatar_hashes(cli, &mut outcomes).await;
    outcomes
}

async fn attach_avatar_hashes(cli: &Cli, outcomes: &mut [CheckOutcome]) {
    if !cli.avatar_hash {
        return;
    }

    let client = match avatar_hash_client(cli) {
        Ok(client) => client,
        Err(err) => {
            tracing::warn!(error = %err, "avatar hashing disabled: failed to build HTTP client");
            return;
        }
    };

    attach_avatar_hashes_with_client(&client, outcomes).await;
}

async fn attach_avatar_hashes_with_client(client: &reqwest::Client, outcomes: &mut [CheckOutcome]) {
    let candidates = avatar_hash_candidates(outcomes);
    let mut results = stream::iter(candidates)
        .map(|candidate| async move {
            let result =
                fetch_avatar_hash(client, &candidate.avatar_url, AvatarHashOptions::default())
                    .await;
            (candidate, result)
        })
        .buffer_unordered(AVATAR_HASH_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    results.sort_by_key(|(candidate, _)| candidate.outcome_index);

    for (candidate, result) in results {
        match result {
            Ok(hash) => {
                let Some(outcome) = outcomes.get_mut(candidate.outcome_index) else {
                    continue;
                };
                let access_path = avatar_hash_access_path(outcome);
                outcome
                    .enrichment
                    .insert("avatar_hash".to_owned(), hash.clone());
                outcome
                    .profile_evidence
                    .push(ProfileEvidence::from_avatar_hash(
                        &outcome.site,
                        &outcome.url,
                        &hash,
                        now_ms(),
                        access_path,
                    ));
                outcome.refresh_confidence();
            }
            Err(err) => {
                tracing::debug!(
                    site = %candidate.site,
                    avatar_url = %candidate.avatar_url,
                    error = %err,
                    "avatar hash skipped"
                );
            }
        }
    }
}

struct AvatarHashCandidate {
    outcome_index: usize,
    site: String,
    avatar_url: String,
}

fn avatar_hash_candidates(outcomes: &[CheckOutcome]) -> Vec<AvatarHashCandidate> {
    outcomes
        .iter()
        .enumerate()
        .filter(|(_, outcome)| outcome.kind == MatchKind::Found)
        .filter(|(_, outcome)| {
            !outcome
                .profile_evidence
                .iter()
                .any(|evidence| evidence.kind == ProfileEvidenceKind::AvatarHash)
        })
        .filter_map(|(outcome_index, outcome)| {
            avatar_url(outcome).map(|avatar_url| AvatarHashCandidate {
                outcome_index,
                site: outcome.site.clone(),
                avatar_url,
            })
        })
        .collect()
}

fn avatar_hash_client(cli: &Cli) -> reqwest::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(concat!("adler/", env!("CARGO_PKG_VERSION"), " avatar-hash"))
        .timeout(AvatarHashOptions::default().timeout)
        .redirect(reqwest::redirect::Policy::limited(3));
    let proxy = if cli.tor {
        Some(TOR_PROXY)
    } else {
        cli.proxy.as_deref()
    };
    if let Some(proxy) = proxy {
        builder = builder.proxy(reqwest::Proxy::all(proxy)?);
    }
    builder.build()
}

fn avatar_url(outcome: &CheckOutcome) -> Option<String> {
    outcome
        .profile_evidence
        .iter()
        .find(|evidence| evidence.kind == ProfileEvidenceKind::AvatarUrl)
        .map(|evidence| evidence.value.clone())
        .or_else(|| outcome.enrichment.get("avatar").cloned())
}

fn avatar_hash_access_path(outcome: &CheckOutcome) -> Option<EvidenceAccessPath> {
    outcome
        .profile_evidence
        .iter()
        .find_map(|evidence| evidence.source.access_path.clone())
        .or_else(|| {
            outcome
                .transport
                .map(|transport| EvidenceAccessPath::new(transport, outcome.escalations, false))
        })
}

fn now_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
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

    let use_cache = !cli.no_cache && !cli.enrich && !cli.correlate && !cli.avatar_hash;
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

#[cfg(test)]
mod tests {
    use super::*;
    use adler_core::MatchKind;
    use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
    use std::collections::BTreeMap;
    use std::io::Cursor;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

    fn png_bytes() -> Vec<u8> {
        let image = RgbImage::from_fn(16, 16, |x, y| {
            if (x + y) % 2 == 0 {
                Rgb([255, 255, 255])
            } else {
                Rgb([0, 0, 0])
            }
        });
        let mut cursor = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(image)
            .write_to(&mut cursor, ImageFormat::Png)
            .unwrap();
        cursor.into_inner()
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

    #[tokio::test]
    async fn attach_avatar_hashes_adds_derived_evidence_without_raw_image_bytes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/avatar.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(png_bytes()),
            )
            .mount(&server)
            .await;

        let mut outcome = outcome("Example", MatchKind::Found);
        let avatar_url = format!("{}/avatar.png", server.uri());
        outcome
            .enrichment
            .insert("avatar".into(), avatar_url.clone());
        outcome
            .profile_evidence
            .push(ProfileEvidence::from_enrichment(
                "Example",
                &outcome.url,
                "avatar",
                &avatar_url,
            ));
        outcome.refresh_confidence();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .unwrap();
        let mut outcomes = vec![outcome];

        attach_avatar_hashes_with_client(&client, &mut outcomes).await;

        let avatar_hash = outcomes[0]
            .profile_evidence
            .iter()
            .find(|evidence| evidence.kind == ProfileEvidenceKind::AvatarHash)
            .expect("avatar hash evidence");
        assert!(avatar_hash.value.starts_with("dhash64_v1:"));
        assert_eq!(
            avatar_hash.source.origin,
            adler_core::EvidenceOrigin::Derived
        );
        assert_eq!(
            outcomes[0].enrichment.get("avatar_hash"),
            Some(&avatar_hash.value)
        );

        let json = serde_json::to_string(&outcomes[0]).unwrap();
        assert!(json.contains("avatar_hash"));
        assert!(!json.contains("PNG"));
    }

    #[tokio::test]
    async fn attach_avatar_hashes_applies_unordered_results_to_original_outcomes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/slow.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_delay(Duration::from_millis(25))
                    .set_body_bytes(png_bytes()),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/fast.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(png_bytes()),
            )
            .mount(&server)
            .await;

        let mut slow = outcome("Slow", MatchKind::Found);
        slow.enrichment
            .insert("avatar".into(), format!("{}/slow.png", server.uri()));
        let mut fast = outcome("Fast", MatchKind::Found);
        fast.enrichment
            .insert("avatar".into(), format!("{}/fast.png", server.uri()));
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .unwrap();
        let mut outcomes = vec![slow, fast];

        attach_avatar_hashes_with_client(&client, &mut outcomes).await;

        assert_eq!(outcomes[0].site, "Slow");
        assert_eq!(outcomes[1].site, "Fast");
        assert!(
            outcomes[0]
                .profile_evidence
                .iter()
                .any(|evidence| evidence.kind == ProfileEvidenceKind::AvatarHash)
        );
        assert!(
            outcomes[1]
                .profile_evidence
                .iter()
                .any(|evidence| evidence.kind == ProfileEvidenceKind::AvatarHash)
        );
    }
}
