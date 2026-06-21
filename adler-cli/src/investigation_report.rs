//! Investigation reports built on `adler-core`'s report model.

use std::io::Write;
use std::path::{Path, PathBuf};

use adler_core::{
    InvestigationReport, build_identity_clusters, render_investigation_report_html,
    render_investigation_report_markdown,
};
use adler_server::{PersistedScan, build_investigation_report};
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
    /// Self-contained HTML case file.
    Html,
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
        ReportFormat::Html => out
            .write_all(render_investigation_report_html(report).as_bytes())
            .context("writing HTML report"),
    }
}

fn report_from_scan(dir: &Path, mut scan: PersistedScan) -> InvestigationReport {
    refresh_scan(&mut scan);
    let related_scans = load_related_scans(dir, &scan.username);
    build_investigation_report(scan, &related_scans)
}

fn load_scan(dir: &Path, scan_id: &str) -> Result<PersistedScan> {
    let path = scan_path(dir, scan_id);
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice::<PersistedScan>(&bytes)
        .with_context(|| format!("parsing persisted scan {}", path.display()))
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
    render_investigation_report_markdown(report)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use adler_core::{
        CheckOutcome, ConfidenceScore, MatchKind, ProfileEvidence, ReportDisabledSite,
        ReportLimitation, ReportLimitationKind, ReportTimelineEvent, ReportTimelineEventKind,
        TransportTier, build_identity_clusters,
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

- Schema version: 4
- Report model: 4
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
    fn report_scan_applies_historical_confidence_overlay_without_rewriting_json() {
        let dir = tempdir().unwrap();
        let mut older = persisted("older", "alice", vec![found("GitHub", "https://alice.dev")]);
        older.created_at_ms = 1_000;
        let mut previous = persisted(
            "previous",
            "alice",
            vec![found("GitHub", "https://alice.dev")],
        );
        previous.created_at_ms = 2_000;
        let mut current = persisted(
            "current",
            "alice",
            vec![found("GitHub", "https://alice.dev")],
        );
        current.created_at_ms = 3_000;

        for scan in [&older, &previous, &current] {
            std::fs::write(
                dir.path().join(format!("{}.json", scan.scan_id)),
                serde_json::to_vec(scan).unwrap(),
            )
            .unwrap();
        }
        let current_path = dir.path().join("current.json");
        let before = std::fs::read(&current_path).unwrap();

        let mut out = Vec::new();
        run_report_scan(Some(dir.path()), "current", ReportFormat::Json, &mut out).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();

        let reasons = json["found_accounts"][0]["confidence"]["reasons"]
            .as_array()
            .unwrap();
        assert!(
            reasons.iter().any(|reason| {
                reason["kind"] == "historical_consistency" && reason["count"] == 2
            })
        );
        let after = std::fs::read(current_path).unwrap();
        assert_eq!(before, after);
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

        assert_eq!(json["schema_version"], 4);
        assert_eq!(json["username"], "alice");
        assert_eq!(json["summary"]["found"], 2);
        assert_eq!(json["found_accounts"].as_array().unwrap().len(), 2);
        assert_eq!(json["identity_clusters"].as_array().unwrap().len(), 1);
        assert_eq!(json["evidence_table"].as_array().unwrap().len(), 2);
        assert_eq!(json["timeline"].as_array().unwrap().len(), 1);
        assert_eq!(json["limitations"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn html_report_serializes_case_file_model() {
        let outcomes = vec![
            found("GitHub", "https://alice.dev"),
            found("GitLab", "https://alice.dev"),
        ];
        let report = InvestigationReport::from_scan(
            "alice<script>",
            &outcomes,
            build_identity_clusters("alice", &outcomes),
        );

        let mut out = Vec::new();
        write_report(&report, ReportFormat::Html, &mut out).unwrap();
        let html = String::from_utf8(out).unwrap();

        assert!(html.contains("<!doctype html>"));
        assert!(html.contains("<h2>Summary</h2>"));
        assert!(html.contains("<h2>High-Confidence Accounts</h2>"));
        assert!(html.contains("<h2>Identity Clusters</h2>"));
        assert!(html.contains("<h2>Evidence Table</h2>"));
        assert!(html.contains("<h2>Timeline</h2>"));
        assert!(html.contains("<h2>Limitations</h2>"));
        assert!(html.contains("alice&lt;script&gt;"));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<img"));
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
