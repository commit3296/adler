//! Markdown investigation reports built on `adler-core`'s report model.

use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};

use adler_core::{
    ClusterReason, ConfidenceLabel, ConfidenceReason, ConfidenceScore,
    INVESTIGATION_REPORT_SCHEMA_VERSION, IdentityCluster, InvestigationReport, MatchKind,
    ProfileEvidenceKind, ReportDisabledSite, ReportLimitation, ReportLimitationKind,
    ReportTimelineEvent, ReportTimelineEventKind, TransportTier, UncertainReason,
    build_identity_clusters,
};
use adler_server::{PersistedScan, TimelineEvent, TimelineEventKind, build_scan_timeline};
use anyhow::{Context as _, Result, bail};
use clap::ValueEnum;

/// Output format for persisted-scan investigation reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum ReportFormat {
    /// Human-readable Markdown report.
    #[default]
    Markdown,
    /// Machine-readable `InvestigationReport` JSON.
    Json,
}

/// Generate an investigation report from a persisted scan id.
pub(crate) fn run_report_scan(
    scans_dir: Option<&Path>,
    scan_id: &str,
    format: ReportFormat,
    out: &mut impl Write,
) -> Result<()> {
    validate_scan_id(scan_id)?;
    let dir = scans_dir.map_or_else(adler_server::default_scans_dir, Path::to_path_buf);
    let scan = load_scan(&dir, scan_id)?;
    let report = report_from_scan(&dir, scan);
    write_report(&report, format, out)
}

fn write_report(
    report: &InvestigationReport,
    format: ReportFormat,
    out: &mut impl Write,
) -> Result<()> {
    match format {
        ReportFormat::Markdown => out
            .write_all(render_markdown(report).as_bytes())
            .context("writing Markdown report"),
        ReportFormat::Json => {
            serde_json::to_writer_pretty(&mut *out, report).context("writing JSON report")?;
            writeln!(out).context("writing JSON report newline")
        }
    }
}

fn report_from_scan(dir: &Path, mut scan: PersistedScan) -> InvestigationReport {
    refresh_scan(&mut scan);
    let timeline = load_report_timeline(dir, &scan);
    let disabled_sites = scan
        .request_context
        .as_ref()
        .map(|context| {
            context
                .disabled_matches
                .iter()
                .map(|site| ReportDisabledSite {
                    name: site.name.clone(),
                    url: site.url.clone(),
                    tags: site.tags.clone(),
                    disabled_reason: site.disabled_reason.clone(),
                })
                .collect()
        })
        .unwrap_or_default();

    InvestigationReport::builder(scan.username, &scan.outcomes)
        .identity_clusters(scan.identity_clusters)
        .timeline(timeline)
        .disabled_sites(disabled_sites)
        .build()
}

fn load_scan(dir: &Path, scan_id: &str) -> Result<PersistedScan> {
    let path = scan_path(dir, scan_id);
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice::<PersistedScan>(&bytes)
        .with_context(|| format!("parsing persisted scan {}", path.display()))
}

fn load_report_timeline(dir: &Path, current: &PersistedScan) -> Vec<ReportTimelineEvent> {
    let mut scans = load_related_scans(dir, &current.username);
    if !scans.iter().any(|scan| scan.scan_id == current.scan_id) {
        scans.push(current.clone());
    }
    let timeline = build_scan_timeline(&scans);
    timeline
        .events
        .into_iter()
        .map(report_timeline_event)
        .collect()
}

fn load_related_scans(dir: &Path, username: &str) -> Vec<PersistedScan> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("json"))
        .filter_map(|path| std::fs::read(path).ok())
        .filter_map(|bytes| serde_json::from_slice::<PersistedScan>(&bytes).ok())
        .filter(|scan| scan.username == username)
        .map(|mut scan| {
            refresh_scan(&mut scan);
            scan
        })
        .collect()
}

fn report_timeline_event(event: TimelineEvent) -> ReportTimelineEvent {
    ReportTimelineEvent {
        kind: match event.kind {
            TimelineEventKind::FirstSeen => ReportTimelineEventKind::AddedFound,
            TimelineEventKind::Disappeared => ReportTimelineEventKind::RemovedFound,
            TimelineEventKind::Reappeared => ReportTimelineEventKind::Reappeared,
            TimelineEventKind::EvidenceChanged => ReportTimelineEventKind::EvidenceChanged,
        },
        site: Some(event.site),
        scan_id: Some(event.scan_id.to_string()),
        observed_at_ms: Some(event.at_ms),
        detail: Some(timeline_detail(event.before, event.after)),
    }
}

fn timeline_detail(before: Option<MatchKind>, after: Option<MatchKind>) -> String {
    match (before, after) {
        (Some(before), Some(after)) => format!("{} -> {}", kind_label(before), kind_label(after)),
        (None, Some(after)) => format!("new {}", kind_label(after)),
        (Some(before), None) => format!("after {}", kind_label(before)),
        (None, None) => "changed".to_owned(),
    }
}

fn refresh_scan(scan: &mut PersistedScan) {
    for outcome in &mut scan.outcomes {
        outcome.refresh_confidence();
    }
    scan.identity_clusters = build_identity_clusters(&scan.username, &scan.outcomes);
}

fn scan_path(dir: &Path, scan_id: &str) -> PathBuf {
    dir.join(format!("{scan_id}.json"))
}

fn validate_scan_id(scan_id: &str) -> Result<()> {
    if scan_id.is_empty()
        || scan_id.contains('/')
        || scan_id.contains('\\')
        || scan_id == "."
        || scan_id == ".."
    {
        bail!("invalid scan id: {scan_id}");
    }
    Ok(())
}

/// Render a deterministic Markdown report. Public inside the crate for unit
/// tests; CLI users reach it through `--report-scan`.
pub(crate) fn render_markdown(report: &InvestigationReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Adler investigation report: {}", report.username);
    let _ = writeln!(out);
    push_summary(&mut out, report);
    push_accounts(&mut out, report);
    push_clusters(&mut out, &report.identity_clusters);
    push_uncertain(&mut out, report);
    push_evidence(&mut out, report);
    push_timeline(&mut out, report);
    push_disabled(&mut out, report);
    push_limitations(&mut out, report);
    out
}

fn push_summary(out: &mut String, report: &InvestigationReport) {
    let summary = &report.summary;
    let _ = writeln!(out, "## Summary");
    let _ = writeln!(out);
    let _ = writeln!(out, "- Schema version: {}", report.schema_version);
    let _ = writeln!(out, "- Report model: {INVESTIGATION_REPORT_SCHEMA_VERSION}");
    let _ = writeln!(
        out,
        "- Outcomes: {} total, {} found, {} not found, {} uncertain",
        summary.total, summary.found, summary.not_found, summary.uncertain
    );
    let _ = writeln!(
        out,
        "- Evidence: {} found with profile evidence, {} evidence items",
        summary.found_with_profile_evidence, summary.profile_evidence_items
    );
    let _ = writeln!(
        out,
        "- Identity clusters: {} total, {} uncertain, {} clustered profiles",
        summary.identity_clusters, summary.uncertain_identity_clusters, summary.clustered_profiles
    );
    let _ = writeln!(out, "- Timeline events: {}", summary.timeline_events);
    let _ = writeln!(out, "- Disabled/parked sites: {}", summary.disabled_sites);
    if let Some(generated_at_ms) = report.generated_at_ms {
        let _ = writeln!(out, "- Generated from scan timestamp: {generated_at_ms}");
    }
    let _ = writeln!(out);
}

fn push_accounts(out: &mut String, report: &InvestigationReport) {
    let _ = writeln!(out, "## High-Confidence Accounts");
    let _ = writeln!(out);
    if report.high_confidence_accounts.is_empty() {
        let _ = writeln!(out, "No high-confidence accounts.");
    } else {
        push_account_table(out, &report.high_confidence_accounts);
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "## Found Accounts");
    let _ = writeln!(out);
    if report.found_accounts.is_empty() {
        let _ = writeln!(out, "No found accounts.");
    } else {
        push_account_table(out, &report.found_accounts);
    }
    let _ = writeln!(out);
}

fn push_account_table(out: &mut String, accounts: &[adler_core::ReportAccount]) {
    let _ = writeln!(
        out,
        "| Site | URL | Confidence | Transport | Cluster | Evidence |"
    );
    let _ = writeln!(out, "| --- | --- | --- | --- | --- | --- |");
    for account in accounts {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} |",
            cell(&account.site),
            link_cell(&account.url),
            cell(&confidence_text(&account.confidence)),
            cell(&transport_text(account.transport, account.escalations)),
            cell(&join_or_dash(&account.cluster_ids)),
            account.profile_evidence.len()
        );
    }
}

fn push_clusters(out: &mut String, clusters: &[IdentityCluster]) {
    let _ = writeln!(out, "## Identity Clusters");
    let _ = writeln!(out);
    if clusters.is_empty() {
        let _ = writeln!(out, "No identity clusters.");
        let _ = writeln!(out);
        return;
    }
    for cluster in clusters {
        let uncertainty = if cluster.uncertain { " uncertain" } else { "" };
        let _ = writeln!(
            out,
            "- `{}`: {}%{}",
            cluster.id, cluster.confidence, uncertainty
        );
        if !cluster.reasons.is_empty() {
            let reasons = cluster
                .reasons
                .iter()
                .map(cluster_reason_text)
                .collect::<Vec<_>>()
                .join("; ");
            let _ = writeln!(out, "  - Reasons: {}", md_text(&reasons));
        }
        for member in &cluster.members {
            let _ = writeln!(
                out,
                "  - {}: {} ({})",
                md_text(&member.site),
                md_text(&member.url),
                confidence_text(&member.confidence)
            );
        }
    }
    let _ = writeln!(out);
}

fn push_uncertain(out: &mut String, report: &InvestigationReport) {
    let _ = writeln!(out, "## Uncertain Accounts");
    let _ = writeln!(out);
    if report.uncertain_accounts.is_empty() {
        let _ = writeln!(out, "No uncertain accounts.");
        let _ = writeln!(out);
        return;
    }
    let _ = writeln!(out, "| Site | URL | Reason | Confidence |");
    let _ = writeln!(out, "| --- | --- | --- | --- |");
    for account in &report.uncertain_accounts {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            cell(&account.site),
            link_cell(&account.url),
            cell(
                &account
                    .reason
                    .as_ref()
                    .map_or_else(|| "unknown".to_owned(), uncertain_text)
            ),
            cell(&confidence_text(&account.confidence))
        );
    }
    let _ = writeln!(out);
}

fn push_evidence(out: &mut String, report: &InvestigationReport) {
    let _ = writeln!(out, "## Evidence Table");
    let _ = writeln!(out);
    if report.evidence_table.is_empty() {
        let _ = writeln!(out, "No structured profile evidence.");
        let _ = writeln!(out);
        return;
    }
    let _ = writeln!(out, "| Site | Kind | Field | Value | Source URL |");
    let _ = writeln!(out, "| --- | --- | --- | --- | --- |");
    for evidence in &report.evidence_table {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            cell(&evidence.site),
            cell(evidence_kind(evidence.kind)),
            cell(evidence.field.as_deref().unwrap_or("")),
            cell(&evidence.value),
            link_cell(&evidence.source.url)
        );
    }
    let _ = writeln!(out);
}

fn push_timeline(out: &mut String, report: &InvestigationReport) {
    let _ = writeln!(out, "## Timeline");
    let _ = writeln!(out);
    if report.timeline.is_empty() {
        let _ = writeln!(out, "No timeline events.");
        let _ = writeln!(out);
        return;
    }
    let _ = writeln!(out, "| At ms | Kind | Site | Scan | Detail |");
    let _ = writeln!(out, "| --- | --- | --- | --- | --- |");
    for event in &report.timeline {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            event
                .observed_at_ms
                .map_or_else(|| "-".to_owned(), |value| value.to_string()),
            cell(timeline_kind(event.kind)),
            cell(event.site.as_deref().unwrap_or("")),
            cell(event.scan_id.as_deref().unwrap_or("")),
            cell(event.detail.as_deref().unwrap_or(""))
        );
    }
    let _ = writeln!(out);
}

fn push_disabled(out: &mut String, report: &InvestigationReport) {
    let _ = writeln!(out, "## Parked Or Disabled Sites");
    let _ = writeln!(out);
    if report.disabled_sites.is_empty() {
        let _ = writeln!(out, "No matching disabled sites recorded.");
        let _ = writeln!(out);
        return;
    }
    let _ = writeln!(out, "| Site | URL | Tags | Reason |");
    let _ = writeln!(out, "| --- | --- | --- | --- |");
    for site in &report.disabled_sites {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            cell(&site.name),
            cell(&site.url),
            cell(&join_or_dash(&site.tags)),
            cell(&site.disabled_reason)
        );
    }
    let _ = writeln!(out);
}

fn push_limitations(out: &mut String, report: &InvestigationReport) {
    let _ = writeln!(out, "## Limitations");
    let _ = writeln!(out);
    if report.limitations.is_empty() {
        let _ = writeln!(out, "No limitations recorded.");
        return;
    }
    for limitation in &report.limitations {
        let _ = writeln!(out, "- {}", limitation_text(limitation));
    }
}

fn confidence_text(confidence: &ConfidenceScore) -> String {
    let reasons = confidence
        .reasons
        .iter()
        .map(confidence_reason_text)
        .collect::<Vec<_>>();
    let base = format!(
        "{} {}%",
        confidence_label(confidence.label),
        confidence.score
    );
    if reasons.is_empty() {
        base
    } else {
        format!("{base} ({})", reasons.join("; "))
    }
}

fn confidence_label(label: ConfidenceLabel) -> &'static str {
    match label {
        ConfidenceLabel::Low => "low",
        ConfidenceLabel::Medium => "medium",
        ConfidenceLabel::High => "high",
        ConfidenceLabel::Verified => "verified",
    }
}

fn confidence_reason_text(reason: &ConfidenceReason) -> String {
    match reason {
        ConfidenceReason::FoundBySignal => "found by signal".to_owned(),
        ConfidenceReason::NotFoundBySignal => "not found by signal".to_owned(),
        ConfidenceReason::ProfileMetadataExtracted { count } => {
            format!("{count} profile metadata field(s)")
        }
        ConfidenceReason::ProfileMetadataRich { count } => {
            format!("{count} rich profile metadata field(s)")
        }
        ConfidenceReason::SignalEvidence { count } => {
            format!("{count} signal evidence line(s)")
        }
        ConfidenceReason::AuthenticatedAccess => "authenticated access".to_owned(),
        ConfidenceReason::BrowserTransport => "browser transport".to_owned(),
        ConfidenceReason::ImpersonateTransport => "impersonate transport".to_owned(),
        ConfidenceReason::EscalatedTransport => "escalated transport".to_owned(),
        ConfidenceReason::WeakStatusOnly => "weak status-only signal".to_owned(),
        ConfidenceReason::UncertainOutcome => "uncertain outcome".to_owned(),
        ConfidenceReason::SessionRequired => "session required".to_owned(),
        ConfidenceReason::TransportBlocked => "transport blocked".to_owned(),
    }
}

fn cluster_reason_text(reason: &ClusterReason) -> String {
    match reason {
        ClusterReason::SharedDisplayName { value } => format!("shared display name: {value}"),
        ClusterReason::SharedBioPhrase { phrase } => format!("shared bio phrase: {phrase}"),
        ClusterReason::SharedExternalLink { value } => format!("shared external link: {value}"),
        ClusterReason::SharedLocation { value } => format!("shared location: {value}"),
        ClusterReason::SharedAvatarUrl { value } => format!("shared avatar URL: {value}"),
        ClusterReason::HistoricalCoOccurrence => "historical co-occurrence".to_owned(),
    }
}

fn limitation_text(limitation: &ReportLimitation) -> String {
    let mut text = match limitation.kind {
        ReportLimitationKind::LowConfidenceFound => "low-confidence found account".to_owned(),
        ReportLimitationKind::MissingProfileEvidence => "missing profile evidence".to_owned(),
        ReportLimitationKind::UncertainOutcome => "uncertain outcome".to_owned(),
        ReportLimitationKind::SessionRequired => "operator session required".to_owned(),
        ReportLimitationKind::GeoUnavailable => "required geo unavailable".to_owned(),
        ReportLimitationKind::Captcha => "CAPTCHA blocked probing".to_owned(),
        ReportLimitationKind::RateLimited => "rate limit blocked probing".to_owned(),
        ReportLimitationKind::BrowserBudget => "browser budget exhausted".to_owned(),
        ReportLimitationKind::TransportBlocked => "transport blocked reliable probing".to_owned(),
        ReportLimitationKind::DisabledSiteOmitted => "disabled/parked site omitted".to_owned(),
    };
    if let Some(site) = &limitation.site {
        let _ = write!(text, " on {site}");
    }
    if let Some(detail) = &limitation.detail {
        let _ = write!(text, ": {detail}");
    }
    md_text(&text)
}

fn uncertain_text(reason: &UncertainReason) -> String {
    reason.to_string()
}

fn kind_label(kind: MatchKind) -> &'static str {
    match kind {
        MatchKind::Found => "found",
        MatchKind::NotFound => "not_found",
        MatchKind::Uncertain => "uncertain",
    }
}

fn timeline_kind(kind: ReportTimelineEventKind) -> &'static str {
    match kind {
        ReportTimelineEventKind::AddedFound => "added_found",
        ReportTimelineEventKind::RemovedFound => "removed_found",
        ReportTimelineEventKind::VerdictChanged => "verdict_changed",
        ReportTimelineEventKind::EvidenceChanged => "evidence_changed",
        ReportTimelineEventKind::Reappeared => "reappeared",
    }
}

fn evidence_kind(kind: ProfileEvidenceKind) -> &'static str {
    match kind {
        ProfileEvidenceKind::DisplayName => "display_name",
        ProfileEvidenceKind::Bio => "bio",
        ProfileEvidenceKind::AvatarUrl => "avatar_url",
        ProfileEvidenceKind::ExternalLink => "external_link",
        ProfileEvidenceKind::Location => "location",
        ProfileEvidenceKind::JoinedDate => "joined_date",
        ProfileEvidenceKind::ProfileTitle => "profile_title",
        ProfileEvidenceKind::MetaDescription => "meta_description",
        ProfileEvidenceKind::ExtractedField => "extracted_field",
    }
}

fn transport_text(transport: Option<TransportTier>, escalations: u8) -> String {
    let transport = transport.map_or("unknown", TransportTier::as_str);
    if escalations == 0 {
        transport.to_owned()
    } else {
        format!("{transport} (+{escalations} escalation)")
    }
}

fn join_or_dash(values: &[String]) -> String {
    if values.is_empty() {
        "-".to_owned()
    } else {
        values.join(", ")
    }
}

fn cell(value: &str) -> String {
    let value = md_text(value.trim());
    if value.is_empty() {
        "-".to_owned()
    } else {
        value
    }
}

fn link_cell(url: &str) -> String {
    let value = url.trim();
    if value.is_empty() {
        "-".to_owned()
    } else {
        format!("<{}>", value.replace('>', "%3E"))
    }
}

fn md_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use adler_core::{
        CheckOutcome, ConfidenceScore, MatchKind, ProfileEvidence, ReportLimitationKind,
        build_identity_clusters,
    };
    use adler_server::{PersistedScan, ScanId, Summary};
    use tempfile::tempdir;

    use super::*;

    fn found(site: &str, website: &str) -> CheckOutcome {
        let url = format!("https://{}.example/alice", site.to_lowercase());
        let mut outcome = CheckOutcome {
            site: site.to_owned(),
            url: url.clone(),
            kind: MatchKind::Found,
            reason: None,
            elapsed_ms: 12,
            enrichment: BTreeMap::new(),
            evidence: vec!["HTTP 200 (status_found)".to_owned()],
            profile_evidence: vec![ProfileEvidence::from_enrichment(
                site, &url, "website", website,
            )],
            confidence: ConfidenceScore::default(),
            transport: Some(TransportTier::Http),
            escalations: 0,
        };
        outcome.refresh_confidence();
        outcome
    }

    fn persisted(scan_id: &str, username: &str, outcomes: Vec<CheckOutcome>) -> PersistedScan {
        PersistedScan {
            schema_version: 2,
            scan_id: ScanId::from(scan_id.to_owned()),
            username: username.to_owned(),
            request_context: None,
            site_count: outcomes.len(),
            created_at_ms: 1_781_192_451_000,
            summary: Summary::from_outcomes(&outcomes),
            identity_clusters: build_identity_clusters(username, &outcomes),
            outcomes,
            elapsed_ms: 42,
        }
    }

    #[test]
    fn markdown_renders_report_sections() {
        let outcomes = vec![
            found("GitHub", "https://alice.dev"),
            found("GitLab", "https://alice.dev"),
        ];
        let report = InvestigationReport::from_scan(
            "alice",
            &outcomes,
            build_identity_clusters("alice", &outcomes),
        );
        let markdown = render_markdown(&report);

        assert!(markdown.contains("# Adler investigation report: alice"));
        assert!(markdown.contains("## High-Confidence Accounts"));
        assert!(markdown.contains("## Identity Clusters"));
        assert!(markdown.contains("identity-0001"));
        assert!(markdown.contains("shared external link"));
        assert!(markdown.contains("## Evidence Table"));
    }

    #[test]
    fn markdown_escapes_table_cells() {
        assert_eq!(cell("a|b\nc"), "a\\|b c");
        assert_eq!(
            link_cell("https://example.test/a>b"),
            "<https://example.test/a%3Eb>"
        );
    }

    #[test]
    fn report_scan_reads_persisted_scan_and_outputs_markdown() {
        let dir = tempdir().unwrap();
        let scan = persisted(
            "scan123",
            "alice",
            vec![
                found("GitHub", "https://alice.dev"),
                found("GitLab", "https://alice.dev"),
            ],
        );
        std::fs::write(
            dir.path().join("scan123.json"),
            serde_json::to_vec(&scan).unwrap(),
        )
        .unwrap();

        let mut out = Vec::new();
        run_report_scan(
            Some(dir.path()),
            "scan123",
            ReportFormat::Markdown,
            &mut out,
        )
        .unwrap();
        let markdown = String::from_utf8(out).unwrap();

        insta::assert_snapshot!(markdown.trim_end(), @r###"
# Adler investigation report: alice

## Summary

- Schema version: 1
- Report model: 1
- Outcomes: 2 total, 2 found, 0 not found, 0 uncertain
- Evidence: 2 found with profile evidence, 2 evidence items
- Identity clusters: 1 total, 0 uncertain, 2 clustered profiles
- Timeline events: 2
- Disabled/parked sites: 0

## High-Confidence Accounts

| Site | URL | Confidence | Transport | Cluster | Evidence |
| --- | --- | --- | --- | --- | --- |
| GitHub | <https://github.example/alice> | high 85% (found by signal; 1 signal evidence line(s); 1 profile metadata field(s)) | http | identity-0001 | 1 |
| GitLab | <https://gitlab.example/alice> | high 85% (found by signal; 1 signal evidence line(s); 1 profile metadata field(s)) | http | identity-0001 | 1 |

## Found Accounts

| Site | URL | Confidence | Transport | Cluster | Evidence |
| --- | --- | --- | --- | --- | --- |
| GitHub | <https://github.example/alice> | high 85% (found by signal; 1 signal evidence line(s); 1 profile metadata field(s)) | http | identity-0001 | 1 |
| GitLab | <https://gitlab.example/alice> | high 85% (found by signal; 1 signal evidence line(s); 1 profile metadata field(s)) | http | identity-0001 | 1 |

## Identity Clusters

- `identity-0001`: 90%
  - Reasons: shared external link: https://alice.dev/
  - GitHub: https://github.example/alice (high 85% (found by signal; 1 signal evidence line(s); 1 profile metadata field(s)))
  - GitLab: https://gitlab.example/alice (high 85% (found by signal; 1 signal evidence line(s); 1 profile metadata field(s)))

## Uncertain Accounts

No uncertain accounts.

## Evidence Table

| Site | Kind | Field | Value | Source URL |
| --- | --- | --- | --- | --- |
| GitHub | external_link | website | https://alice.dev | <https://github.example/alice> |
| GitLab | external_link | website | https://alice.dev | <https://gitlab.example/alice> |

## Timeline

| At ms | Kind | Site | Scan | Detail |
| --- | --- | --- | --- | --- |
| 1781192451000 | added_found | GitHub | scan123 | new found |
| 1781192451000 | added_found | GitLab | scan123 | new found |

## Parked Or Disabled Sites

No matching disabled sites recorded.

## Limitations

No limitations recorded.
"###);
        assert!(markdown.contains("# Adler investigation report: alice"));
        assert!(markdown.contains("GitHub"));
        assert!(markdown.contains("identity-0001"));
        assert!(markdown.contains("## Timeline"));
    }

    #[test]
    fn json_report_serializes_core_model() {
        let outcomes = vec![
            found("GitHub", "https://alice.dev"),
            found("GitLab", "https://alice.dev"),
        ];
        let clusters = build_identity_clusters("alice", &outcomes);
        let timeline = vec![ReportTimelineEvent {
            kind: ReportTimelineEventKind::AddedFound,
            site: Some("GitHub".to_owned()),
            scan_id: Some("scan123".to_owned()),
            observed_at_ms: Some(1_781_192_451_000),
            detail: Some("new found".to_owned()),
        }];
        let disabled = ReportDisabledSite {
            name: "Threads".to_owned(),
            url: "https://threads.net/@{username}".to_owned(),
            tags: vec!["social".to_owned()],
            disabled_reason: "login wall".to_owned(),
        };
        let report = InvestigationReport::builder("alice", &outcomes)
            .identity_clusters(clusters)
            .timeline(timeline)
            .disabled_sites(vec![disabled])
            .build();

        let mut out = Vec::new();
        write_report(&report, ReportFormat::Json, &mut out).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();

        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["username"], "alice");
        assert_eq!(json["summary"]["found"], 2);
        assert_eq!(json["found_accounts"].as_array().unwrap().len(), 2);
        assert_eq!(json["identity_clusters"].as_array().unwrap().len(), 1);
        assert_eq!(json["evidence_table"].as_array().unwrap().len(), 2);
        assert_eq!(json["timeline"].as_array().unwrap().len(), 1);
        assert_eq!(json["limitations"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn invalid_scan_ids_are_rejected() {
        let dir = tempdir().unwrap();
        let err = run_report_scan(
            Some(dir.path()),
            "../scan",
            ReportFormat::Markdown,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid scan id"));
    }

    #[test]
    fn limitations_are_rendered() {
        let mut report = InvestigationReport::from_scan("alice", &[], Vec::new());
        report.limitations.push(ReportLimitation {
            kind: ReportLimitationKind::MissingProfileEvidence,
            site: Some("GitHub".to_owned()),
            detail: None,
        });
        let markdown = render_markdown(&report);

        assert!(markdown.contains("missing profile evidence on GitHub"));
    }
}
