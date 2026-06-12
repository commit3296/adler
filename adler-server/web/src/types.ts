// Wire types — kept in lock-step with `adler-server`'s serde
// definitions. If a server type changes, this file is the single
// place to update on the frontend side.

export interface SiteSummary {
    name: string;
    url: string;
    tags: string[];
    popularity?: number;
}

export interface DisabledSiteSummary extends SiteSummary {
    disabled_reason: string;
}

export interface SitesResponse {
    sites: SiteSummary[];
    disabled: DisabledSiteSummary[];
}

export type UncertainReason =
    | "rate_limited"
    | "cloudflare_challenge"
    | "captcha"
    | "robots_disallowed"
    | "deadline"
    | "scheduler_closed"
    | "browser_budget"
    | "username_not_allowed"
    | "geo_unavailable"
    | "session_required"
    | { network: string }
    | { body_read: string }
    | { browser_failed: string }
    | { other: string };

export type MatchKind = "found" | "not_found" | "uncertain";

/// Which transport produced an outcome. Stamped by the router on every
/// probe so the UI can show whether HTTP was enough, whether
/// impersonation was needed, or whether the scan reached for the
/// browser. Older persisted scans may omit it.
export type TransportTier = "http" | "impersonate" | "browser";

export type ProfileEvidenceKind =
    | "username"
    | "display_name"
    | "bio"
    | "avatar_url"
    | "external_link"
    | "location"
    | "joined_date"
    | "profile_title"
    | "meta_description"
    | "extracted_field";

export interface ProfileEvidence {
    kind: ProfileEvidenceKind;
    field?: string;
    value: string;
    source: {
        site: string;
        url: string;
        origin: "signal" | "extractor";
        observed_at_ms?: number;
        access_path?: EvidenceAccessPath;
    };
}

export interface EvidenceAccessPath {
    transport: TransportTier;
    escalated?: boolean;
    authenticated?: boolean;
    session_required?: boolean;
}

export type ConfidenceLabel = "low" | "medium" | "high" | "verified";

export interface ConfidenceScore {
    score: number;
    label: ConfidenceLabel;
    reasons?: Array<
        | { kind: "found_by_signal" }
        | { kind: "not_found_by_signal" }
        | { kind: "profile_metadata_extracted"; count: number }
        | { kind: "profile_metadata_rich"; count: number }
        | { kind: "signal_evidence"; count: number }
        | { kind: "exact_username_match"; count: number }
        | { kind: "historical_consistency"; count: number }
        | { kind: "authenticated_access" }
        | { kind: "browser_transport" }
        | { kind: "impersonate_transport" }
        | { kind: "escalated_transport" }
        | { kind: "weak_status_only" }
        | { kind: "uncertain_outcome" }
        | { kind: "session_required" }
        | { kind: "transport_blocked" }
    >;
}

export interface CheckOutcome {
    site: string;
    url: string;
    kind: MatchKind;
    reason?: UncertainReason;
    elapsed_ms: number;
    enrichment?: Record<string, string>;
    evidence?: string[];
    profile_evidence?: ProfileEvidence[];
    confidence?: ConfidenceScore;
    /// Which transport (HTTP / impersonate / browser) produced this
    /// verdict. Missing on older persisted scans.
    transport?: TransportTier;
    /// Automatic escalations beyond the primary route — typically 0,
    /// 1 when the cheap path's `Uncertain(cloudflare_challenge |
    /// rate_limited)` was retried through the browser.
    escalations?: number;
}

export interface VerdictChange {
    site: string;
    before: MatchKind;
    after: MatchKind;
}

export interface EvidenceChange {
    site: string;
    before_enrichment?: Record<string, string>;
    after_enrichment?: Record<string, string>;
    before_profile_evidence?: ProfileEvidence[];
    after_profile_evidence?: ProfileEvidence[];
}

export interface ScanDiff {
    from_scan_id: string;
    to_scan_id: string;
    added_found?: CheckOutcome[];
    removed_found?: CheckOutcome[];
    verdict_changes?: VerdictChange[];
    evidence_changes?: EvidenceChange[];
}

export interface Summary {
    found: number;
    not_found: number;
    uncertain: number;
}

export type ClusterReason =
    | { kind: "shared_display_name"; value: string }
    | { kind: "shared_bio_phrase"; phrase: string }
    | { kind: "shared_external_link"; value: string }
    | { kind: "shared_location"; value: string }
    | { kind: "shared_avatar_url"; value: string }
    | { kind: "historical_co_occurrence" };

export interface ObservedProfile {
    site: string;
    username: string;
    url: string;
    evidence?: ProfileEvidence[];
    confidence: ConfidenceScore;
    observed_at_ms?: number;
}

export interface IdentityCluster {
    id: string;
    members: ObservedProfile[];
    confidence: number;
    reasons?: ClusterReason[];
    uncertain: boolean;
}

export interface ReportSummary {
    total: number;
    found: number;
    not_found: number;
    uncertain: number;
    high_confidence_found: number;
    found_with_profile_evidence: number;
    profile_evidence_items: number;
    identity_clusters: number;
    uncertain_identity_clusters: number;
    clustered_profiles: number;
    timeline_events: number;
    disabled_sites: number;
}

export interface ReportAccount {
    site: string;
    url: string;
    confidence: ConfidenceScore;
    signal_evidence?: string[];
    profile_evidence?: ProfileEvidence[];
    cluster_ids?: string[];
    transport?: TransportTier;
    escalations?: number;
    elapsed_ms: number;
}

export interface ReportUncertainAccount {
    site: string;
    url: string;
    reason?: UncertainReason;
    confidence: ConfidenceScore;
    transport?: TransportTier;
    escalations?: number;
    elapsed_ms: number;
}

export interface ReportEvidence {
    site: string;
    url: string;
    kind: ProfileEvidenceKind;
    field?: string;
    value: string;
    source: ProfileEvidence["source"];
}

export type ReportTimelineEventKind =
    | "added_found"
    | "removed_found"
    | "verdict_changed"
    | "evidence_changed"
    | "reappeared";

export interface ReportTimelineEvent {
    kind: ReportTimelineEventKind;
    site?: string;
    scan_id?: string;
    observed_at_ms?: number;
    detail?: string;
}

export interface ReportDisabledSite {
    name: string;
    url: string;
    tags?: string[];
    disabled_reason: string;
}

export type ReportLimitationKind =
    | "low_confidence_found"
    | "missing_profile_evidence"
    | "uncertain_outcome"
    | "session_required"
    | "geo_unavailable"
    | "captcha"
    | "rate_limited"
    | "browser_budget"
    | "transport_blocked"
    | "disabled_site_omitted";

export interface ReportLimitation {
    kind: ReportLimitationKind;
    site?: string;
    detail?: string;
}

export interface InvestigationReport {
    schema_version: number;
    username: string;
    generated_at_ms?: number;
    summary: ReportSummary;
    found_accounts?: ReportAccount[];
    high_confidence_accounts?: ReportAccount[];
    uncertain_accounts?: ReportUncertainAccount[];
    evidence_table?: ReportEvidence[];
    identity_clusters?: IdentityCluster[];
    timeline?: ReportTimelineEvent[];
    disabled_sites?: ReportDisabledSite[];
    limitations?: ReportLimitation[];
}

export interface FinishedScan {
    summary: Summary;
    outcomes: CheckOutcome[];
    elapsed_ms: number;
    identity_clusters?: IdentityCluster[];
}

export type ScanSnapshot =
    | {
          status: "running";
          username: string;
          site_count: number;
          elapsed_ms: number;
          partial: CheckOutcome[];
      }
    | {
          status: "finished";
          username: string;
          site_count: number;
          summary: Summary;
          outcomes: CheckOutcome[];
          elapsed_ms: number;
          identity_clusters?: IdentityCluster[];
      };

export interface ScanListEntry {
    scan_id: string;
    username: string;
    site_count: number;
    started_at_ms: number;
    elapsed_ms: number;
    status: "running" | "finished";
    summary?: Summary;
}

export interface StartScanResponse {
    scan_id: string;
    username: string;
    site_count: number;
}

export interface ApiError {
    error: string;
    message: string;
    disabled_matches?: DisabledSiteSummary[];
}

export type EgressKind = "datacenter" | "residential" | "mobile" | "tor";

/// Read-only view of one configured egress proxy. Proxy URLs are
/// deliberately absent — they typically embed credentials and have no
/// business reaching the browser.
export interface EgressSummary {
    name?: string;
    country?: string;
    kind: EgressKind;
}

/// `GET /api/access` payload — what's configured via `--proxy-pool` and
/// `--sessions`, *without* secrets. Editing happens out-of-band by
/// updating the TOML files and restarting the server.
export interface AccessResponse {
    egress: EgressSummary[];
    sessions: { name: string }[];
}

export interface StartScanBody {
    username: string;
    only?: string[];
    exclude?: string[];
    tag?: string[];
    exclude_tag?: string[];
    top?: number;
    nsfw?: boolean;
    concurrency?: number;
    deadline_secs?: number;
    /// Subset of the loaded `--proxy-pool` to route through for this
    /// scan, selected by `name`. Empty / omitted = full pool.
    egress_names?: string[];
}

export interface RetryResponse {
    outcome: CheckOutcome;
}

/// Body for `POST /api/scan/:id/refilter`. Same shape as
/// `StartScanBody` minus `username` (carried over from the existing
/// scan).
export interface RefilterBody {
    only?: string[];
    exclude?: string[];
    tag?: string[];
    exclude_tag?: string[];
    top?: number;
    nsfw?: boolean;
    concurrency?: number;
    deadline_secs?: number;
    egress_names?: string[];
}

export interface RefilterResponse {
    scan_id: string;
    derived_from: string;
    carried_outcomes: number;
    site_count: number;
}

/// Helper: extract the reason "tag" regardless of whether it arrived
/// as a bare string (unit variant) or an externally-tagged object.
export function reasonTag(r: UncertainReason | undefined): string | undefined {
    if (r === undefined) return undefined;
    if (typeof r === "string") return r;
    return Object.keys(r)[0];
}
