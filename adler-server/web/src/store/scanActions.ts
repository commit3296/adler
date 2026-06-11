import { produce, reconcile, type SetStoreFunction } from "solid-js/store";

import { ApiClientError, api } from "../api";
import type { CheckOutcome, IdentityCluster, Summary } from "../types";
import type {
    AppStore,
    NotFoundState,
    ScanFilterSnapshot,
    ScanState,
} from "../store";

type ToastKind = "success" | "error" | "info";
type LoadableScanState = Omit<ScanState, "identityClusters"> & {
    identityClusters?: IdentityCluster[];
};

interface Deps {
    set: SetStoreFunction<AppStore>;
    store: AppStore;
    emptyBuckets: () => Record<string, CheckOutcome[]>;
    bucketsFrom: (
        outcomes: CheckOutcome[],
        categoryBySite: Record<string, string>,
    ) => Record<string, CheckOutcome[]>;
    toast: (text: string, kind?: ToastKind) => void;
}

export function createScanActions({
    set,
    store,
    emptyBuckets,
    bucketsFrom,
    toast,
}: Deps) {
    function setRetrying(site: string, on: boolean) {
        set(
            "ui",
            "retrying",
            produce((r: Record<string, true>) => {
                if (on) r[site] = true;
                else delete r[site];
            }),
        );
    }

    function replaceOutcome(o: CheckOutcome) {
        if (!store.scan) return;
        const catId = store.categoryBySite[o.site] ?? "other";
        set(
            "scan",
            produce((s: ScanState | null) => {
                if (!s) return;
                const idx = s.outcomes.findIndex((x) => x.site === o.site);
                if (idx >= 0) s.outcomes[idx] = o;
                else {
                    s.outcomes.push(o);
                    s.outcomeSites[o.site] = true;
                }
                // Replace in bucket too. Same `o` reference goes into
                // both arrays so equality holds.
                const bucket = (s.bucketsByCategory[catId] ??= []);
                const bIdx = bucket.findIndex((x) => x.site === o.site);
                if (bIdx >= 0) bucket[bIdx] = o;
                else bucket.push(o);

                let f = 0;
                let nf = 0;
                let u = 0;
                for (const x of s.outcomes) {
                    if (x.kind === "found") f++;
                    else if (x.kind === "not_found") nf++;
                    else u++;
                }
                s.summary = { found: f, not_found: nf, uncertain: u };
            }),
        );
    }

    return {
        beginScan(
            id: string,
            username: string,
            siteCount: number,
            filterAtStart: ScanFilterSnapshot,
        ) {
            set("diff", null);
            set("notFound", null);
            set("loading", false);
            set("scan", {
                id,
                username,
                filterAtStart,
                outcomes: [],
                outcomeSites: {},
                bucketsByCategory: emptyBuckets(),
                identityClusters: [],
                status: "running",
                summary: null,
                siteCount,
                startedAtMs: Date.now(),
                elapsedMs: 0,
            });
        },
        rebindScanAfterRefilter(
            id: string,
            siteCount: number,
            filterAtStart: ScanFilterSnapshot,
        ) {
            if (!store.scan) return;
            set("scan", "id", id);
            set("scan", "siteCount", siteCount);
            set("scan", "outcomes", []);
            set("scan", "outcomeSites", reconcile({}));
            set("scan", "bucketsByCategory", reconcile(emptyBuckets()));
            set("scan", "identityClusters", []);
            set("scan", "summary", null);
            set("scan", "status", "running");
            set("scan", "startedAtMs", Date.now());
            set("scan", "elapsedMs", 0);
            set("scan", "filterAtStart", filterAtStart);
        },
        appendOutcome(o: CheckOutcome) {
            if (!store.scan) return;
            // O(1) dedupe — the server replays its full history when SSE
            // re-connects (after Stop → Continue), so this check is hot.
            if (store.scan.outcomeSites[o.site]) return;
            const catId = store.categoryBySite[o.site] ?? "other";
            set(
                "scan",
                produce((s: ScanState | null) => {
                    if (!s) return;
                    s.outcomeSites[o.site] = true;
                    s.outcomes.push(o);
                    (s.bucketsByCategory[catId] ??= []).push(o);
                }),
            );
        },
        appendOutcomes(list: CheckOutcome[]) {
            if (!store.scan || list.length === 0) return;
            const catIds: string[] = list.map(
                (o) => store.categoryBySite[o.site] ?? "other",
            );
            set(
                "scan",
                produce((s: ScanState | null) => {
                    if (!s) return;
                    for (let i = 0; i < list.length; i++) {
                        const o = list[i]!;
                        if (s.outcomeSites[o.site]) continue;
                        s.outcomeSites[o.site] = true;
                        s.outcomes.push(o);
                        const catId = catIds[i]!;
                        (s.bucketsByCategory[catId] ??= []).push(o);
                    }
                }),
            );
        },
        replaceOutcome,
        setRetrying,
        async retrySite(site: string) {
            if (!store.scan) return;
            const scanId = store.scan.id;
            setRetrying(site, true);
            try {
                const r = await api.retrySite(scanId, site);
                // Guard: another scan may have loaded mid-flight.
                if (store.scan?.id === scanId) {
                    replaceOutcome(r.outcome);
                    if (r.outcome.kind === "found") {
                        toast(`${site}: found`, "success");
                    } else if (r.outcome.kind === "not_found") {
                        toast(`${site}: not found`, "info");
                    } else {
                        toast(`${site}: still uncertain`, "info");
                    }
                }
            } catch (e) {
                const msg = e instanceof ApiClientError ? e.message : String(e);
                toast(`Retry failed: ${msg}`, "error");
            } finally {
                setRetrying(site, false);
            }
        },
        finishScan(
            summary: Summary,
            outcomes: CheckOutcome[],
            elapsedMs: number,
            identityClusters: IdentityCluster[] = [],
        ) {
            const sites = Object.fromEntries(
                outcomes.map((o) => [o.site, true as const]),
            );
            const buckets = bucketsFrom(outcomes, store.categoryBySite);
            set(
                "scan",
                produce((s: ScanState | null) => {
                    if (!s) return;
                    s.status = "finished";
                    s.summary = summary;
                    s.outcomes = outcomes;
                    s.outcomeSites = sites;
                    s.bucketsByCategory = buckets;
                    s.identityClusters = identityClusters;
                    s.elapsedMs = elapsedMs;
                }),
            );
        },
        loadScan(scan: LoadableScanState) {
            set("diff", null);
            set("notFound", null);
            set("loading", false);
            scan.identityClusters ??= [];
            if (!scan.outcomeSites || Object.keys(scan.outcomeSites).length === 0) {
                scan.outcomeSites = Object.fromEntries(
                    scan.outcomes.map((o) => [o.site, true as const]),
                );
            }
            if (
                !scan.bucketsByCategory ||
                Object.keys(scan.bucketsByCategory).length === 0
            ) {
                scan.bucketsByCategory = bucketsFrom(
                    scan.outcomes,
                    store.categoryBySite,
                );
            }
            set("scan", scan as ScanState);
        },
        clearScan() {
            set("scan", null);
            set("diff", null);
            set("notFound", null);
            set("loading", false);
        },
        setNotFound(nf: NotFoundState | null) {
            if (nf) {
                set("scan", null);
                set("diff", null);
                set("loading", false);
            }
            set("notFound", nf);
        },
        setLoading(on: boolean) {
            set("loading", on);
        },
        setServerVersion(v: string) {
            set("serverVersion", v);
        },
        tickElapsed() {
            if (!store.scan || store.scan.status !== "running") return;
            set("scan", "elapsedMs", Date.now() - store.scan.startedAtMs);
        },
        pauseScan() {
            if (!store.scan || store.scan.status !== "running") return;
            set("scan", "status", "stopped");
        },
        resumeScan() {
            if (!store.scan || store.scan.status !== "stopped") return;
            set("scan", "status", "running");
        },
    };
}
