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
    | { network: string }
    | { body_read: string }
    | { browser_failed: string }
    | { other: string };

export type MatchKind = "found" | "not_found" | "uncertain";

export interface CheckOutcome {
    site: string;
    url: string;
    kind: MatchKind;
    reason?: UncertainReason;
    elapsed_ms: number;
    enrichment?: Record<string, string>;
    evidence?: string[];
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
