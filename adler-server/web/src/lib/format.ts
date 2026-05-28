// Small presentation helpers shared across components.

/// Human-friendly "X ago" string for a Unix epoch millisecond timestamp.
export function fmtAgo(ms: number): string {
    const s = Math.max(0, Math.floor((Date.now() - ms) / 1000));
    if (s < 5) return "just now";
    if (s < 60) return `${s}s ago`;
    if (s < 3600) return `${Math.floor(s / 60)}m ago`;
    return `${Math.floor(s / 3600)}h ago`;
}

/// Strip a "detection endpoint" wrapper around a profile URL — e.g.
/// Pinterest's oembed.json?url=<profile> — so the user sees the page
/// they can actually visit.
export function displayUrl(raw: string): string {
    try {
        const u = new URL(raw);
        const inner = u.searchParams.get("url");
        if (inner && /^https?:\/\//.test(inner)) return inner;
    } catch {
        /* not a parseable URL — pass through */
    }
    return raw;
}

export function fmtElapsed(ms: number): string {
    return `${(ms / 1000).toFixed(1)}s`;
}
