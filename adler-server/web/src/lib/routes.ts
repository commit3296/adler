const ROUTE_VIEW_RE = /^#\/(scan|diff)\//;
const SCAN_ROUTE_RE = /^#\/scan\/([a-z0-9]+)/;
const DIFF_ROUTE_RE = /^#\/diff\/([a-z0-9]+)\/([a-z0-9]+)/;

export function routeHasScanView(hash: string): boolean {
    return ROUTE_VIEW_RE.test(hash);
}

export function isHomeHash(hash: string): boolean {
    return hash === "" || hash === "#" || hash === "#/";
}

export function scanIdFromHash(hash: string): string | null {
    return hash.match(SCAN_ROUTE_RE)?.[1] ?? null;
}

export function diffIdsFromHash(hash: string): [string, string] | null {
    const match = hash.match(DIFF_ROUTE_RE);
    return match ? [match[1]!, match[2]!] : null;
}
