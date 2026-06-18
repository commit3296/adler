//! Deterministic renderers for investigation reports.
//!
//! The JSON contract is the `InvestigationReport` serde shape. These helpers
//! provide presentation formats for local case files without pulling in CLI,
//! Web, or MCP dependencies.

use std::fmt::Write as _;

use crate::check::UncertainReason;
use crate::confidence::{ConfidenceLabel, ConfidenceReason, ConfidenceScore};
use crate::escalation::TransportTier;
use crate::identity::{ClusterReason, IdentityCluster};
use crate::profile::ProfileEvidenceKind;
use crate::report::{
    INVESTIGATION_REPORT_SCHEMA_VERSION, InvestigationReport, ReportAccount, ReportLimitation,
    ReportLimitationKind, ReportTimelineEventKind,
};

/// Render a deterministic Markdown investigation report.
#[must_use]
pub fn render_investigation_report_markdown(report: &InvestigationReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Adler investigation report: {}", report.username);
    let _ = writeln!(out);
    push_markdown_summary(&mut out, report);
    push_markdown_accounts(&mut out, report);
    push_markdown_clusters(&mut out, &report.identity_clusters);
    push_markdown_uncertain(&mut out, report);
    push_markdown_evidence(&mut out, report);
    push_markdown_timeline(&mut out, report);
    push_markdown_disabled(&mut out, report);
    push_markdown_limitations(&mut out, report);
    out
}

/// Render a self-contained, no-JS HTML investigation report.
#[must_use]
pub fn render_investigation_report_html(report: &InvestigationReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "<!doctype html>");
    let _ = writeln!(out, "<html lang=\"en\">");
    let _ = writeln!(out, "<head>");
    let _ = writeln!(out, "<meta charset=\"utf-8\">");
    let _ = writeln!(
        out,
        "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">"
    );
    let _ = writeln!(
        out,
        "<title>Adler investigation report: {}</title>",
        html_text(&report.username)
    );
    let _ = writeln!(out, "<style>{REPORT_CSS}</style>");
    let _ = writeln!(out, "</head>");
    let _ = writeln!(out, "<body>");
    let _ = writeln!(out, "<main class=\"report\">");
    let _ = writeln!(
        out,
        "<header class=\"report-header\"><p class=\"eyebrow\">Adler case file</p><h1>Investigation report: {}</h1></header>",
        html_text(&report.username)
    );
    push_html_summary(&mut out, report);
    push_html_accounts(&mut out, report);
    push_html_clusters(&mut out, &report.identity_clusters);
    push_html_uncertain(&mut out, report);
    push_html_evidence(&mut out, report);
    push_html_timeline(&mut out, report);
    push_html_disabled(&mut out, report);
    push_html_limitations(&mut out, report);
    let _ = writeln!(out, "</main>");
    let _ = writeln!(out, "</body>");
    let _ = writeln!(out, "</html>");
    out
}

const REPORT_CSS: &str = r#"
:root {
  color-scheme: light;
  --bg: #f7f8fa;
  --panel: #ffffff;
  --text: #18202a;
  --muted: #667085;
  --border: #d9dee7;
  --accent: #116a7b;
  --accent-soft: #e7f4f6;
  --warn-soft: #fff4d8;
  --shadow: 0 1px 2px rgba(16, 24, 40, 0.06);
}
* { box-sizing: border-box; }
body {
  margin: 0;
  background: var(--bg);
  color: var(--text);
  font: 14px/1.55 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}
.report {
  max-width: 1120px;
  margin: 0 auto;
  padding: 32px 20px 48px;
}
.report-header {
  margin-bottom: 24px;
}
.eyebrow {
  margin: 0 0 4px;
  color: var(--accent);
  font-size: 12px;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}
h1, h2, h3 {
  margin: 0;
  line-height: 1.2;
}
h1 {
  font-size: 32px;
}
h2 {
  font-size: 20px;
  margin-bottom: 12px;
}
h3 {
  font-size: 15px;
}
.section {
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: 8px;
  box-shadow: var(--shadow);
  margin-top: 16px;
  padding: 18px;
}
.summary-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
  gap: 10px;
}
.metric {
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 10px;
}
.metric span {
  display: block;
  color: var(--muted);
  font-size: 12px;
}
.metric strong {
  display: block;
  margin-top: 2px;
  font-size: 18px;
}
.table-wrap {
  overflow-x: auto;
}
table {
  border-collapse: collapse;
  width: 100%;
}
th, td {
  border-bottom: 1px solid var(--border);
  padding: 8px 10px;
  text-align: left;
  vertical-align: top;
}
th {
  color: var(--muted);
  font-size: 12px;
  font-weight: 700;
  text-transform: uppercase;
}
td code, .mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 12px;
  overflow-wrap: anywhere;
}
.muted {
  color: var(--muted);
}
.cluster {
  border: 1px solid var(--border);
  border-radius: 8px;
  margin-top: 10px;
  padding: 12px;
}
.cluster-head {
  display: flex;
  align-items: center;
  gap: 8px;
  justify-content: space-between;
}
.badge {
  background: var(--accent-soft);
  border-radius: 999px;
  color: var(--accent);
  display: inline-block;
  font-size: 12px;
  font-weight: 700;
  padding: 2px 8px;
}
.badge.warn {
  background: var(--warn-soft);
  color: #7a4b00;
}
.list {
  margin: 8px 0 0;
  padding-left: 18px;
}
.empty {
  color: var(--muted);
  margin: 0;
}
@media print {
  body { background: #ffffff; }
  .report { max-width: none; padding: 0; }
  .section { box-shadow: none; break-inside: avoid; }
}
"#;

fn push_markdown_summary(out: &mut String, report: &InvestigationReport) {
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

fn push_markdown_accounts(out: &mut String, report: &InvestigationReport) {
    let _ = writeln!(out, "## High-Confidence Accounts");
    let _ = writeln!(out);
    if report.high_confidence_accounts.is_empty() {
        let _ = writeln!(out, "No high-confidence accounts.");
    } else {
        push_markdown_account_table(out, &report.high_confidence_accounts);
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "## Found Accounts");
    let _ = writeln!(out);
    if report.found_accounts.is_empty() {
        let _ = writeln!(out, "No found accounts.");
    } else {
        push_markdown_account_table(out, &report.found_accounts);
    }
    let _ = writeln!(out);
}

fn push_markdown_account_table(out: &mut String, accounts: &[ReportAccount]) {
    let _ = writeln!(
        out,
        "| Site | URL | Confidence | Transport | Cluster | Evidence |"
    );
    let _ = writeln!(out, "| --- | --- | --- | --- | --- | --- |");
    for account in accounts {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} |",
            markdown_cell(&account.site),
            markdown_link_cell(&account.url),
            markdown_cell(&confidence_text(&account.confidence)),
            markdown_cell(&transport_text(account.transport, account.escalations)),
            markdown_cell(&join_or_dash(&account.cluster_ids)),
            account.profile_evidence.len()
        );
    }
}

fn push_markdown_clusters(out: &mut String, clusters: &[IdentityCluster]) {
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
            let _ = writeln!(out, "  - Reasons: {}", markdown_text(&reasons));
        }
        for member in &cluster.members {
            let _ = writeln!(
                out,
                "  - {}: {} ({})",
                markdown_text(&member.site),
                markdown_text(&member.url),
                confidence_text(&member.confidence)
            );
        }
    }
    let _ = writeln!(out);
}

fn push_markdown_uncertain(out: &mut String, report: &InvestigationReport) {
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
            markdown_cell(&account.site),
            markdown_link_cell(&account.url),
            markdown_cell(
                &account
                    .reason
                    .as_ref()
                    .map_or_else(|| "unknown".to_owned(), uncertain_text)
            ),
            markdown_cell(&confidence_text(&account.confidence))
        );
    }
    let _ = writeln!(out);
}

fn push_markdown_evidence(out: &mut String, report: &InvestigationReport) {
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
            markdown_cell(&evidence.site),
            markdown_cell(evidence_kind(evidence.kind)),
            markdown_cell(evidence.field.as_deref().unwrap_or("")),
            markdown_cell(&evidence.value),
            markdown_link_cell(&evidence.source.url)
        );
    }
    let _ = writeln!(out);
}

fn push_markdown_timeline(out: &mut String, report: &InvestigationReport) {
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
            markdown_cell(timeline_kind(event.kind)),
            markdown_cell(event.site.as_deref().unwrap_or("")),
            markdown_cell(event.scan_id.as_deref().unwrap_or("")),
            markdown_cell(event.detail.as_deref().unwrap_or(""))
        );
    }
    let _ = writeln!(out);
}

fn push_markdown_disabled(out: &mut String, report: &InvestigationReport) {
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
            markdown_cell(&site.name),
            markdown_cell(&site.url),
            markdown_cell(&join_or_dash(&site.tags)),
            markdown_cell(&site.disabled_reason)
        );
    }
    let _ = writeln!(out);
}

fn push_markdown_limitations(out: &mut String, report: &InvestigationReport) {
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

fn push_html_summary(out: &mut String, report: &InvestigationReport) {
    let summary = &report.summary;
    let _ = writeln!(out, "<section class=\"section\"><h2>Summary</h2>");
    let _ = writeln!(out, "<div class=\"summary-grid\">");
    push_html_metric(out, "Schema version", &report.schema_version.to_string());
    push_html_metric(
        out,
        "Report model",
        &INVESTIGATION_REPORT_SCHEMA_VERSION.to_string(),
    );
    push_html_metric(out, "Total outcomes", &summary.total.to_string());
    push_html_metric(out, "Found", &summary.found.to_string());
    push_html_metric(out, "Not found", &summary.not_found.to_string());
    push_html_metric(out, "Uncertain", &summary.uncertain.to_string());
    push_html_metric(
        out,
        "Found with evidence",
        &summary.found_with_profile_evidence.to_string(),
    );
    push_html_metric(
        out,
        "Evidence items",
        &summary.profile_evidence_items.to_string(),
    );
    push_html_metric(
        out,
        "Identity clusters",
        &summary.identity_clusters.to_string(),
    );
    push_html_metric(
        out,
        "Uncertain clusters",
        &summary.uncertain_identity_clusters.to_string(),
    );
    push_html_metric(
        out,
        "Clustered profiles",
        &summary.clustered_profiles.to_string(),
    );
    push_html_metric(out, "Timeline events", &summary.timeline_events.to_string());
    push_html_metric(out, "Disabled sites", &summary.disabled_sites.to_string());
    if let Some(generated_at_ms) = report.generated_at_ms {
        push_html_metric(
            out,
            "Generated from scan timestamp",
            &generated_at_ms.to_string(),
        );
    }
    let _ = writeln!(out, "</div></section>");
}

fn push_html_metric(out: &mut String, label: &str, value: &str) {
    let _ = writeln!(
        out,
        "<div class=\"metric\"><span>{}</span><strong>{}</strong></div>",
        html_text(label),
        html_text(value)
    );
}

fn push_html_accounts(out: &mut String, report: &InvestigationReport) {
    push_html_account_section(
        out,
        "High-Confidence Accounts",
        "No high-confidence accounts.",
        &report.high_confidence_accounts,
    );
    push_html_account_section(
        out,
        "Found Accounts",
        "No found accounts.",
        &report.found_accounts,
    );
}

fn push_html_account_section(
    out: &mut String,
    title: &str,
    empty_text: &str,
    accounts: &[ReportAccount],
) {
    let _ = writeln!(
        out,
        "<section class=\"section\"><h2>{}</h2>",
        html_text(title)
    );
    if accounts.is_empty() {
        let _ = writeln!(out, "<p class=\"empty\">{}</p>", html_text(empty_text));
        let _ = writeln!(out, "</section>");
        return;
    }
    let _ = writeln!(out, "<div class=\"table-wrap\"><table>");
    let _ = writeln!(
        out,
        "<thead><tr><th>Site</th><th>URL</th><th>Confidence</th><th>Transport</th><th>Cluster</th><th>Evidence</th></tr></thead><tbody>"
    );
    for account in accounts {
        let _ = writeln!(
            out,
            "<tr><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            html_value(&account.site),
            html_value(&account.url),
            html_value(&confidence_text(&account.confidence)),
            html_value(&transport_text(account.transport, account.escalations)),
            html_value(&join_or_dash(&account.cluster_ids)),
            account.profile_evidence.len()
        );
    }
    let _ = writeln!(out, "</tbody></table></div></section>");
}

fn push_html_clusters(out: &mut String, clusters: &[IdentityCluster]) {
    let _ = writeln!(out, "<section class=\"section\"><h2>Identity Clusters</h2>");
    if clusters.is_empty() {
        let _ = writeln!(out, "<p class=\"empty\">No identity clusters.</p>");
        let _ = writeln!(out, "</section>");
        return;
    }
    for cluster in clusters {
        let _ = writeln!(out, "<article class=\"cluster\">");
        let _ = writeln!(
            out,
            "<div class=\"cluster-head\"><h3><code>{}</code></h3><span class=\"badge\">{}%</span></div>",
            html_text(&cluster.id),
            cluster.confidence
        );
        if cluster.uncertain {
            let _ = writeln!(out, "<span class=\"badge warn\">uncertain</span>");
        }
        if !cluster.reasons.is_empty() {
            let _ = writeln!(out, "<ul class=\"list\">");
            for reason in &cluster.reasons {
                let _ = writeln!(out, "<li>{}</li>", html_text(&cluster_reason_text(reason)));
            }
            let _ = writeln!(out, "</ul>");
        }
        let _ = writeln!(out, "<div class=\"table-wrap\"><table>");
        let _ = writeln!(
            out,
            "<thead><tr><th>Site</th><th>Username</th><th>URL</th><th>Confidence</th></tr></thead><tbody>"
        );
        for member in &cluster.members {
            let _ = writeln!(
                out,
                "<tr><td>{}</td><td>{}</td><td><code>{}</code></td><td>{}</td></tr>",
                html_value(&member.site),
                html_value(&member.username),
                html_value(&member.url),
                html_value(&confidence_text(&member.confidence))
            );
        }
        let _ = writeln!(out, "</tbody></table></div>");
        let _ = writeln!(out, "</article>");
    }
    let _ = writeln!(out, "</section>");
}

fn push_html_uncertain(out: &mut String, report: &InvestigationReport) {
    let _ = writeln!(
        out,
        "<section class=\"section\"><h2>Uncertain Accounts</h2>"
    );
    if report.uncertain_accounts.is_empty() {
        let _ = writeln!(out, "<p class=\"empty\">No uncertain accounts.</p>");
        let _ = writeln!(out, "</section>");
        return;
    }
    let _ = writeln!(out, "<div class=\"table-wrap\"><table>");
    let _ = writeln!(
        out,
        "<thead><tr><th>Site</th><th>URL</th><th>Reason</th><th>Confidence</th></tr></thead><tbody>"
    );
    for account in &report.uncertain_accounts {
        let reason = account
            .reason
            .as_ref()
            .map_or_else(|| "unknown".to_owned(), uncertain_text);
        let _ = writeln!(
            out,
            "<tr><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td></tr>",
            html_value(&account.site),
            html_value(&account.url),
            html_value(&reason),
            html_value(&confidence_text(&account.confidence))
        );
    }
    let _ = writeln!(out, "</tbody></table></div></section>");
}

fn push_html_evidence(out: &mut String, report: &InvestigationReport) {
    let _ = writeln!(out, "<section class=\"section\"><h2>Evidence Table</h2>");
    if report.evidence_table.is_empty() {
        let _ = writeln!(
            out,
            "<p class=\"empty\">No structured profile evidence.</p>"
        );
        let _ = writeln!(out, "</section>");
        return;
    }
    let _ = writeln!(out, "<div class=\"table-wrap\"><table>");
    let _ = writeln!(
        out,
        "<thead><tr><th>Site</th><th>Kind</th><th>Field</th><th>Value</th><th>Source URL</th></tr></thead><tbody>"
    );
    for evidence in &report.evidence_table {
        let _ = writeln!(
            out,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><code>{}</code></td></tr>",
            html_value(&evidence.site),
            html_value(evidence_kind(evidence.kind)),
            html_value(evidence.field.as_deref().unwrap_or("")),
            html_value(&evidence.value),
            html_value(&evidence.source.url)
        );
    }
    let _ = writeln!(out, "</tbody></table></div></section>");
}

fn push_html_timeline(out: &mut String, report: &InvestigationReport) {
    let _ = writeln!(out, "<section class=\"section\"><h2>Timeline</h2>");
    if report.timeline.is_empty() {
        let _ = writeln!(out, "<p class=\"empty\">No timeline events.</p>");
        let _ = writeln!(out, "</section>");
        return;
    }
    let _ = writeln!(out, "<div class=\"table-wrap\"><table>");
    let _ = writeln!(
        out,
        "<thead><tr><th>At ms</th><th>Kind</th><th>Site</th><th>Scan</th><th>Detail</th></tr></thead><tbody>"
    );
    for event in &report.timeline {
        let at = event
            .observed_at_ms
            .map_or_else(|| "-".to_owned(), |value| value.to_string());
        let _ = writeln!(
            out,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            html_value(&at),
            html_value(timeline_kind(event.kind)),
            html_value(event.site.as_deref().unwrap_or("")),
            html_value(event.scan_id.as_deref().unwrap_or("")),
            html_value(event.detail.as_deref().unwrap_or(""))
        );
    }
    let _ = writeln!(out, "</tbody></table></div></section>");
}

fn push_html_disabled(out: &mut String, report: &InvestigationReport) {
    let _ = writeln!(
        out,
        "<section class=\"section\"><h2>Parked Or Disabled Sites</h2>"
    );
    if report.disabled_sites.is_empty() {
        let _ = writeln!(
            out,
            "<p class=\"empty\">No matching disabled sites recorded.</p>"
        );
        let _ = writeln!(out, "</section>");
        return;
    }
    let _ = writeln!(out, "<div class=\"table-wrap\"><table>");
    let _ = writeln!(
        out,
        "<thead><tr><th>Site</th><th>URL</th><th>Tags</th><th>Reason</th></tr></thead><tbody>"
    );
    for site in &report.disabled_sites {
        let _ = writeln!(
            out,
            "<tr><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td></tr>",
            html_value(&site.name),
            html_value(&site.url),
            html_value(&join_or_dash(&site.tags)),
            html_value(&site.disabled_reason)
        );
    }
    let _ = writeln!(out, "</tbody></table></div></section>");
}

fn push_html_limitations(out: &mut String, report: &InvestigationReport) {
    let _ = writeln!(out, "<section class=\"section\"><h2>Limitations</h2>");
    if report.limitations.is_empty() {
        let _ = writeln!(out, "<p class=\"empty\">No limitations recorded.</p>");
        let _ = writeln!(out, "</section>");
        return;
    }
    let _ = writeln!(out, "<ul class=\"list\">");
    for limitation in &report.limitations {
        let _ = writeln!(out, "<li>{}</li>", html_text(&limitation_text(limitation)));
    }
    let _ = writeln!(out, "</ul></section>");
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
        ConfidenceReason::ExactUsernameMatch { count } => {
            format!("{count} exact username match(es)")
        }
        ConfidenceReason::HistoricalConsistency { count } => {
            format!("{count} stable historical observation(s)")
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
        ClusterReason::SharedAvatarHash { value } => format!("shared avatar hash: {value}"),
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
    markdown_text(&text)
}

fn uncertain_text(reason: &UncertainReason) -> String {
    reason.to_string()
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
        ProfileEvidenceKind::Username => "username",
        ProfileEvidenceKind::DisplayName => "display_name",
        ProfileEvidenceKind::Bio => "bio",
        ProfileEvidenceKind::AvatarUrl => "avatar_url",
        ProfileEvidenceKind::AvatarHash => "avatar_hash",
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

fn markdown_cell(value: &str) -> String {
    let value = markdown_text(value.trim());
    if value.is_empty() {
        "-".to_owned()
    } else {
        value
    }
}

fn markdown_link_cell(url: &str) -> String {
    let value = url.trim();
    if value.is_empty() {
        "-".to_owned()
    } else {
        format!("<{}>", value.replace('>', "%3E"))
    }
}

fn markdown_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\n', '\r'], " ")
}

fn html_value(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        "-".to_owned()
    } else {
        html_text(value)
    }
}

fn html_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            '\n' | '\r' => escaped.push(' '),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        CheckOutcome, ConfidenceScore, MatchKind, ProfileEvidence, ReportDisabledSite,
        ReportLimitation, ReportTimelineEvent, ReportTimelineEventKind, TransportTier,
        build_identity_clusters,
    };

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

    fn report() -> InvestigationReport {
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
        InvestigationReport::builder("alice", &outcomes)
            .identity_clusters(clusters)
            .timeline(timeline)
            .disabled_sites(vec![disabled])
            .build()
    }

    #[test]
    fn markdown_renders_report_sections() {
        let markdown = render_investigation_report_markdown(&report());

        assert!(markdown.contains("# Adler investigation report: alice"));
        assert!(markdown.contains("## High-Confidence Accounts"));
        assert!(markdown.contains("## Identity Clusters"));
        assert!(markdown.contains("identity-0001"));
        assert!(markdown.contains("shared external link"));
        assert!(markdown.contains("## Evidence Table"));
    }

    #[test]
    fn markdown_escapes_table_cells() {
        assert_eq!(markdown_cell("a|b\nc"), "a\\|b c");
        assert_eq!(
            markdown_link_cell("https://example.test/a>b"),
            "<https://example.test/a%3Eb>"
        );
    }

    #[test]
    fn html_renders_report_sections() {
        let html = render_investigation_report_html(&report());

        assert!(html.contains("<!doctype html>"));
        assert!(html.contains("<h2>Summary</h2>"));
        assert!(html.contains("<h2>High-Confidence Accounts</h2>"));
        assert!(html.contains("<h2>Identity Clusters</h2>"));
        assert!(html.contains("identity-0001"));
        assert!(html.contains("shared external link"));
        assert!(html.contains("<h2>Evidence Table</h2>"));
        assert!(html.contains("<h2>Timeline</h2>"));
        assert!(html.contains("<h2>Limitations</h2>"));
        assert!(!html.contains("<script"));
        assert!(!html.contains("<img"));
    }

    #[test]
    fn html_escapes_hostile_report_strings() {
        let mut report = InvestigationReport::from_scan("<alice>", &[], Vec::new());
        report.limitations.push(ReportLimitation {
            kind: ReportLimitationKind::MissingProfileEvidence,
            site: Some("<script>alert(1)</script>".to_owned()),
            detail: Some("\"quoted\" & unsafe".to_owned()),
        });

        let html = render_investigation_report_html(&report);

        assert!(html.contains("&lt;alice&gt;"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.contains("&quot;quoted&quot; &amp; unsafe"));
        assert!(!html.contains("<script>alert(1)</script>"));
    }

    #[test]
    fn html_snapshot_includes_case_file_sections() {
        let html = render_investigation_report_html(&report());
        let outline = html_outline(&html);

        insta::assert_snapshot!(outline, @r###"
<title>Adler investigation report: alice</title>
<header class="report-header"><p class="eyebrow">Adler case file</p><h1>Investigation report: alice</h1></header>
<section class="section"><h2>Summary</h2>
<section class="section"><h2>High-Confidence Accounts</h2>
<section class="section"><h2>Found Accounts</h2>
<section class="section"><h2>Identity Clusters</h2>
<div class="cluster-head"><h3><code>identity-0001</code></h3><span class="badge">90%</span></div>
<li>shared external link: https://alice.dev/</li>
<section class="section"><h2>Uncertain Accounts</h2>
<section class="section"><h2>Evidence Table</h2>
<tr><td>GitHub</td><td>external_link</td><td>website</td><td>https://alice.dev</td><td><code>https://github.example/alice</code></td></tr>
<section class="section"><h2>Timeline</h2>
<tr><td>1781192451000</td><td>added_found</td><td>GitHub</td><td>scan123</td><td>new found</td></tr>
<section class="section"><h2>Parked Or Disabled Sites</h2>
<tr><td>Threads</td><td><code>https://threads.net/@{username}</code></td><td>social</td><td>login wall</td></tr>
<section class="section"><h2>Limitations</h2>
<li>disabled/parked site omitted on Threads: login wall</li>
"###);
    }

    fn html_outline(html: &str) -> String {
        html.lines()
            .filter(|line| {
                line.contains("<title>")
                    || line.contains("<header")
                    || line.contains("<h2>")
                    || line.starts_with("<div class=\"cluster-head\"")
                    || line.contains("shared external link")
                    || line.contains("GitHub</td><td>external_link")
                    || line.contains("1781192451000")
                    || line.contains("Threads</td>")
                    || line.contains("disabled/parked site omitted")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
