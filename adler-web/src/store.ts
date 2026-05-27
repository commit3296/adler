// Centralised app store using `solid-js/store`. Fine-grained
// reactivity means components subscribe only to the slices they read,
// so streaming outcomes update O(1) DOM rows without manual diffing.

import { createEffect, createRoot } from "solid-js";
import { createStore, produce } from "solid-js/store";
import { ApiClientError, api } from "./api";
import { CATEGORIES, categoryForTags, type Preset } from "./constants";
import type {
    CheckOutcome,
    ScanListEntry,
    SiteSummary,
    Summary,
} from "./types";

/// localStorage key. Bumped (`.v1`) when the persisted shape changes.
const PREFS_KEY = "adler.prefs.v1";

/// The subset of state we persist across reloads — non-controversial
/// view preferences and the NSFW toggle. Per-scan filter state (tags,
/// excludeTag, top, presetId) is *not* persisted; it's session-scoped
/// because tag selections feel like part of "the scan I'm running",
/// not "how I like my UI".
interface PersistedPrefs {
    sort: Sort;
    groupBy: GroupBy;
    showNotFound: boolean;
    nsfw: boolean;
}

function loadPrefs(): Partial<PersistedPrefs> {
    try {
        const raw = localStorage.getItem(PREFS_KEY);
        if (!raw) return {};
        const parsed = JSON.parse(raw);
        return typeof parsed === "object" && parsed ? parsed : {};
    } catch {
        return {};
    }
}

function savePrefs(prefs: PersistedPrefs): void {
    try {
        localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
    } catch {
        /* localStorage disabled (private mode / quota) — silent fallback */
    }
}

export type Sort = "status" | "name" | "time";
export type GroupBy = "category" | "none";

/// `running`   — SSE stream is open and outcomes arrive live.
/// `stopped`   — client closed its stream; backend may still be
///                probing (we'll catch up when the user resumes or
///                refreshes via `/api/scan/:id`).
/// `finished`  — backend emitted `done`; the aggregate is final.
export type ScanStatus = "running" | "stopped" | "finished";

export interface ScanState {
    id: string;
    username: string;
    outcomes: CheckOutcome[];
    /// Set-as-record of site names already present in `outcomes`. Used
    /// for O(1) dedupe inside `appendOutcome` — without it the per-push
    /// `.some()` scan grows to O(N²) over the full scan (≈ 3.5M string
    /// comparisons on a 1890-site run).
    outcomeSites: Record<string, true>;
    /// Pre-bucketized outcomes by semantic category. Pushed-to in
    /// `appendOutcomes` synchronously with `outcomes`, so the result
    /// list never has to re-bucketize a 1890-item array on every
    /// rAF. Solid's fine-grained reactivity means only the
    /// CategoryBlock matching the changed bucket re-evaluates.
    bucketsByCategory: Record<string, CheckOutcome[]>;
    status: ScanStatus;
    summary: Summary | null;
    siteCount: number;
    startedAtMs: number;
    elapsedMs: number;
}

function emptyBuckets(): Record<string, CheckOutcome[]> {
    const b: Record<string, CheckOutcome[]> = {};
    for (const cat of CATEGORIES) b[cat.id] = [];
    return b;
}
function bucketsFrom(
    outcomes: CheckOutcome[],
    categoryBySite: Record<string, string>,
): Record<string, CheckOutcome[]> {
    const b = emptyBuckets();
    for (const o of outcomes) {
        const cat = categoryBySite[o.site] ?? "other";
        (b[cat] ??= []).push(o);
    }
    return b;
}

export interface DiffState {
    a: { id: string; username: string; outcomes: CheckOutcome[] };
    b: { id: string; username: string; outcomes: CheckOutcome[] };
}

export interface FilterState {
    presetId: string | null;
    tag: string[];
    excludeTag: string[];
    top: number | null;
    nsfw: boolean;
}

export interface ViewState {
    sort: Sort;
    groupBy: GroupBy;
    resultsFilter: string;
    showNotFound: boolean;
    selectedSite: string | null;
}

export interface UiState {
    drawerOpen: boolean;
    filtersOpen: boolean;
    shortcutsOpen: boolean;
    toast: { text: string; kind: "success" | "error" | "info" } | null;
    compareArmed: string | null;
    /// Set of site names currently being re-probed via the retry
    /// endpoint. The corresponding ResultRow shows a spinner state.
    retrying: Record<string, true>;
}

export interface AppStore {
    catalog: SiteSummary[];
    tagsBySite: Record<string, string[]>;
    /// Pre-computed `site → category.id` so the streaming render
    /// path doesn't re-evaluate `categoryForTags` on every outcome.
    /// Without it, the result list's bucketization is O(K·N) per
    /// outcome — for 1890 sites × 6 categories that's ~11k tag
    /// lookups *per arriving outcome*, ~21M over the whole scan.
    /// With it: O(1) lookup, plain hash equality.
    categoryBySite: Record<string, string>;
    tagCounts: Record<string, number>;
    filter: FilterState;
    scan: ScanState | null;
    diff: DiffState | null;
    history: ScanListEntry[];
    view: ViewState;
    ui: UiState;
}

const persisted = loadPrefs();

const [store, set] = createStore<AppStore>({
    catalog: [],
    tagsBySite: {},
    categoryBySite: {},
    tagCounts: {},
    filter: {
        presetId: "quick",
        tag: [],
        excludeTag: ["bot-protected"],
        top: null,
        nsfw: persisted.nsfw ?? false,
    },
    scan: null,
    diff: null,
    history: [],
    view: {
        sort: persisted.sort ?? "status",
        groupBy: persisted.groupBy ?? "category",
        resultsFilter: "",
        showNotFound: persisted.showNotFound ?? false,
        selectedSite: null,
    },
    ui: {
        drawerOpen: false,
        filtersOpen: false,
        shortcutsOpen: false,
        toast: null,
        compareArmed: null,
        retrying: {},
    },
});

// Persist UI prefs whenever any of the watched slices change. Wrapped
// in createRoot so the effect lives outside any component lifecycle —
// the store is module-scoped, so the effect should be too.
createRoot(() => {
    createEffect(() => {
        savePrefs({
            sort: store.view.sort,
            groupBy: store.view.groupBy,
            showNotFound: store.view.showNotFound,
            nsfw: store.filter.nsfw,
        });
    });
});

export { store, set };

export const actions = {
    setCatalog(sites: SiteSummary[]) {
        const tagsBySite: Record<string, string[]> = {};
        const categoryBySite: Record<string, string> = {};
        const tagCounts: Record<string, number> = {};
        for (const s of sites) {
            tagsBySite[s.name] = s.tags;
            categoryBySite[s.name] = categoryForTags(s.tags).id;
            for (const t of s.tags) tagCounts[t] = (tagCounts[t] ?? 0) + 1;
        }
        set("catalog", sites);
        set("tagsBySite", tagsBySite);
        set("categoryBySite", categoryBySite);
        set("tagCounts", tagCounts);
    },
    setHistory(list: ScanListEntry[]) {
        set("history", list);
    },

    // Filter mutations
    applyPreset(p: Preset) {
        const filter: FilterState = {
            presetId: p.id,
            tag: (p.filter.tag ?? []).slice(),
            excludeTag: (p.filter.exclude_tag ?? []).slice(),
            top: p.filter.top ?? null,
            nsfw: !!p.filter.nsfw,
        };
        set("filter", filter);
    },
    toggleTag(t: string) {
        set("filter", "presetId", null);
        set(
            "filter",
            "tag",
            produce((tags: string[]) => {
                const idx = tags.indexOf(t);
                if (idx >= 0) tags.splice(idx, 1);
                else tags.push(t);
            }),
        );
    },
    removeTag(t: string) {
        set("filter", "presetId", null);
        set("filter", "tag", (tags) => tags.filter((x) => x !== t));
    },
    removeExcludeTag(t: string) {
        set("filter", "presetId", null);
        set("filter", "excludeTag", (tags) => tags.filter((x) => x !== t));
    },
    setTop(n: number | null) {
        set("filter", "presetId", null);
        set("filter", "top", n);
    },
    setNsfw(on: boolean) {
        set("filter", "presetId", null);
        set("filter", "nsfw", on);
    },

    // Scan lifecycle
    beginScan(id: string, username: string, siteCount: number) {
        set("diff", null);
        set("scan", {
            id,
            username,
            outcomes: [],
            outcomeSites: {},
            bucketsByCategory: emptyBuckets(),
            status: "running",
            summary: null,
            siteCount,
            startedAtMs: Date.now(),
            elapsedMs: 0,
        });
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
    /// Append a whole burst of outcomes inside a single Solid reactive
    /// cycle. SSE re-subscribes (and warm-start replays) deliver
    /// dozens of events synchronously; without batching, every memo
    /// downstream of `store.scan.outcomes` (filter, sort, per-category
    /// bucket) re-evaluates *per event*. With batching, all of that
    /// runs exactly once per frame.
    appendOutcomes(list: CheckOutcome[]) {
        if (!store.scan || list.length === 0) return;
        // Pre-compute category ids outside the produce — store reads
        // inside produce go through the reactive proxy and add tracking
        // overhead.
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
    replaceOutcome(o: CheckOutcome) {
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
                // recompute summary
                let f = 0,
                    nf = 0,
                    u = 0;
                for (const x of s.outcomes) {
                    if (x.kind === "found") f++;
                    else if (x.kind === "not_found") nf++;
                    else u++;
                }
                s.summary = { found: f, not_found: nf, uncertain: u };
            }),
        );
    },
    /// Mark / unmark a site as currently being retried (UI-only).
    setRetrying(site: string, on: boolean) {
        set(
            "ui",
            "retrying",
            produce((r: Record<string, true>) => {
                if (on) r[site] = true;
                else delete r[site];
            }),
        );
    },
    /// Re-probe a single site through the server's
    /// `POST /api/scan/:id/retry` endpoint and replace its outcome
    /// in the current scan. No-op if no scan is loaded.
    async retrySite(site: string) {
        if (!store.scan) return;
        const scanId = store.scan.id;
        this.setRetrying(site, true);
        try {
            const r = await api.retrySite(scanId, site);
            // Guard: another scan may have loaded mid-flight.
            if (store.scan?.id === scanId) {
                this.replaceOutcome(r.outcome);
                if (r.outcome.kind === "found") {
                    this.toast(`${site}: found`, "success");
                } else if (r.outcome.kind === "not_found") {
                    this.toast(`${site}: not found`, "info");
                } else {
                    this.toast(`${site}: still uncertain`, "info");
                }
            }
        } catch (e) {
            const msg = e instanceof ApiClientError ? e.message : String(e);
            this.toast(`Retry failed: ${msg}`, "error");
        } finally {
            this.setRetrying(site, false);
        }
    },
    finishScan(summary: Summary, outcomes: CheckOutcome[], elapsedMs: number) {
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
                s.elapsedMs = elapsedMs;
            }),
        );
    },
    loadScan(scan: ScanState) {
        set("diff", null);
        // Backfill derived state when loading historical scans that
        // were serialised before the optimisation existed.
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
        set("scan", scan);
    },
    clearScan() {
        set("scan", null);
        set("diff", null);
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

    // Diff
    setDiff(d: DiffState | null) {
        set("diff", d);
        if (d) set("scan", null);
    },
    armCompare(id: string | null) {
        set("ui", "compareArmed", id);
    },

    // View
    setSort(s: Sort) {
        set("view", "sort", s);
    },
    setGroupBy(g: GroupBy) {
        set("view", "groupBy", g);
    },
    setResultsFilter(s: string) {
        set("view", "resultsFilter", s);
    },
    toggleShowNotFound() {
        set("view", "showNotFound", !store.view.showNotFound);
    },
    selectSite(s: string | null) {
        set("view", "selectedSite", s);
    },

    // UI — only one overlay open at a time. Opening any of
    // {drawer, filters, shortcuts} closes the others. The user
    // never sees two stacked dialogs by accident.
    setDrawer(open: boolean) {
        if (open) {
            set("ui", "filtersOpen", false);
            set("ui", "shortcutsOpen", false);
        }
        set("ui", "drawerOpen", open);
    },
    setFilters(open: boolean) {
        if (open) {
            set("ui", "drawerOpen", false);
            set("ui", "shortcutsOpen", false);
        }
        set("ui", "filtersOpen", open);
    },
    setShortcuts(open: boolean) {
        if (open) {
            set("ui", "drawerOpen", false);
            set("ui", "filtersOpen", false);
        }
        set("ui", "shortcutsOpen", open);
    },
    toast(text: string, kind: "success" | "error" | "info" = "info") {
        set("ui", "toast", { text, kind });
        setTimeout(() => {
            if (store.ui.toast?.text === text) set("ui", "toast", null);
        }, 2200);
    },
    /// Imperatively dismiss the current toast (e.g. on click).
    setToast(t: { text: string; kind: "success" | "error" | "info" } | null) {
        set("ui", "toast", t);
    },
};
