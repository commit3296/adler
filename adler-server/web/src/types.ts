// Wire types — kept in lock-step with `adler-server`'s serde
// definitions. If a server type changes, this file is the single
// place to update on the frontend side.

export interface SiteSummary {
    name: string;
    url: string;
    tags: string[];
    popularity?: number;
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

export interface CheckOutcome {
    site: string;
    url: string;
    kind: MatchKind;
    reason?: UncertainReason;
    elapsed_ms: number;
    enrichment?: Record<string, string>;
    evidence?: string[];
    /// Which transport (HTTP / impersonate / browser) produced this
    /// verdict. Missing on older persisted scans.
    transport?: TransportTier;
    /// Automatic escalations beyond the primary route — typically 0,
    /// 1 when the cheap path's `Uncertain(cloudflare_challenge |
    /// rate_limited)` was retried through the browser.
    escalations?: number;
}

export interface Summary {
    found: number;
    not_found: number;
    uncertain: number;
}

export interface FinishedScan {
    summary: Summary;
    outcomes: CheckOutcome[];
    elapsed_ms: number;
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

/// Helper: extract the reason "tag" regardless of whether it arrived
/// as a bare string (unit variant) or an externally-tagged object.
export function reasonTag(r: UncertainReason | undefined): string | undefined {
    if (r === undefined) return undefined;
    if (typeof r === "string") return r;
    return Object.keys(r)[0];
}
