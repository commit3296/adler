// Typed wrappers around the `adler-server` HTTP API.
//
// Everything goes through the same `fetch` helper so error handling
// is uniform: server-emitted `{ error, message }` envelopes throw
// `ApiClientError` with both fields preserved.

import type {
    AccessResponse,
    CheckOutcome,
    DisabledSiteSummary,
    FinishedScan,
    RefilterBody,
    RefilterResponse,
    RetryResponse,
    ScanDiff,
    ScanListEntry,
    ScanSnapshot,
    SiteSummary,
    SitesResponse,
    StartScanBody,
    StartScanResponse,
} from "./types";

export class ApiClientError extends Error {
    code: string;
    disabledMatches: DisabledSiteSummary[];
    constructor(
        code: string,
        message: string,
        disabledMatches: DisabledSiteSummary[] = [],
    ) {
        super(message);
        this.code = code;
        this.disabledMatches = disabledMatches;
        this.name = "ApiClientError";
    }
}

async function request<T>(input: string, init?: RequestInit): Promise<T> {
    const r = await fetch(input, init);
    if (!r.ok) {
        const body = (await r.json().catch(() => null)) as
            | { error?: string; message?: string }
            | null;
        throw new ApiClientError(
            body?.error ?? "http_error",
            body?.message ?? r.statusText,
            Array.isArray((body as { disabled_matches?: unknown } | null)?.disabled_matches)
                ? ((body as { disabled_matches: DisabledSiteSummary[] }).disabled_matches)
                : [],
        );
    }
    return (await r.json()) as T;
}

async function sites(): Promise<SitesResponse> {
    const payload = await request<SitesResponse | SiteSummary[]>("/api/sites");
    if (Array.isArray(payload)) return { sites: payload, disabled: [] };
    return {
        sites: payload.sites ?? [],
        disabled: payload.disabled ?? [],
    };
}

export const api = {
    health: () => request<{ ok: boolean; version: string }>("/api/health"),
    sites,
    access: () => request<AccessResponse>("/api/access"),
    scans: () => request<ScanListEntry[]>("/api/scans"),
    scanDiff: (from: string, to: string) =>
        request<ScanDiff>(`/api/scans/${from}/diff/${to}`),
    scan: (id: string) => request<ScanSnapshot>(`/api/scan/${id}`),
    startScan: (body: StartScanBody) =>
        request<StartScanResponse>("/api/scan", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(body),
        }),
    retrySite: (id: string, site: string) =>
        request<RetryResponse>(`/api/scan/${id}/retry`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ site }),
        }),
    refilterScan: (id: string, body: RefilterBody) =>
        request<RefilterResponse>(`/api/scan/${id}/refilter`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(body),
        }),
};

export interface SseHandlers {
    onStart?: (data: { username: string }) => void;
    onOutcome: (outcome: CheckOutcome) => void;
    onDone: (finished: FinishedScan) => void;
    onError?: () => void;
}

/// Subscribe to a scan's Server-Sent Events stream. Returns a `close`
/// function that detaches the source.
export function streamScan(id: string, h: SseHandlers): () => void {
    const src = new EventSource(`/api/scan/${id}/stream`);
    src.addEventListener("start", (ev) => {
        try {
            h.onStart?.(JSON.parse((ev as MessageEvent).data));
        } catch {
            /* ignore parse errors — the stream is still useful */
        }
    });
    src.addEventListener("outcome", (ev) => {
        try {
            h.onOutcome(JSON.parse((ev as MessageEvent).data));
        } catch {}
    });
    src.addEventListener("done", (ev) => {
        try {
            h.onDone(JSON.parse((ev as MessageEvent).data));
        } catch {}
        src.close();
    });
    src.addEventListener("error", () => {
        h.onError?.();
    });
    return () => src.close();
}
