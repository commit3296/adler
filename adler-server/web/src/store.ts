// Centralised app store using `solid-js/store`. Fine-grained
// reactivity means components subscribe only to the slices they read,
// so streaming outcomes update O(1) DOM rows without manual diffing.

import { createEffect, createRoot } from "solid-js";
import { createStore } from "solid-js/store";
import { CATEGORIES } from "./constants";
import { createBatchActions } from "./store/batchActions";
import { createCatalogActions } from "./store/catalogActions";
import { createDiffActions } from "./store/diffActions";
import { createFilterActions } from "./store/filterActions";
import { createScanActions } from "./store/scanActions";
import { createUiActions } from "./store/uiActions";
import { createViewActions } from "./store/viewActions";
import type {
    AccessResponse,
    CheckOutcome,
    ScanDiff,
    DisabledSiteSummary,
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

/// Effective server-side filter the scan was started with. Compared
/// against the live `store.filter` to surface divergence — when the
/// operator edits the filter mid-scan we offer a "re-scan with these
/// filters" path via `POST /api/scan/:id/refilter` instead of forcing
/// them to wait, stop, and start over.
export interface ScanFilterSnapshot {
    tag: string[];
    excludeTag: string[];
    top: number | null;
    nsfw: boolean;
    egressNames: string[];
}

export interface ScanState {
    id: string;
    username: string;
    /// Filter the scan was launched with (or refiltered into). Stable
    /// for the duration of the scan — refilter creates a fresh scan
    /// with a new snapshot rather than mutating this in place.
    filterAtStart: ScanFilterSnapshot;
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
    scanDiff?: ScanDiff;
}

/// A view explicitly representing "the thing this URL points at does
/// not exist", rather than silently bouncing home or leaving an empty
/// shell behind a transient toast.
/// - `scan` / `diff`: a `#/scan/:id` / `#/diff/:a/:b` whose id(s) the
///   server reported as `scan_not_found`.
/// - `route`: a hash that matches no known route at all.
export interface NotFoundState {
    kind: "scan" | "diff" | "route";
    detail: string;
}

/// One username in a batch run. `scanId` is filled once its scan is
/// created, `found` once it finishes. Batch runs are sequential, so at
/// most one entry is `running` at a time.
export interface BatchEntry {
    username: string;
    scanId: string | null;
    status: "queued" | "running" | "done" | "error";
    found: number | null;
}

export interface BatchState {
    entries: BatchEntry[];
    running: boolean;
}

export interface FilterState {
    presetId: string | null;
    tag: string[];
    excludeTag: string[];
    top: number | null;
    nsfw: boolean;
    /// Per-scan egress subset — names from the loaded `--proxy-pool`.
    /// Empty = use the full pool. Persists across scans within a
    /// session, but not across reloads (operator usually wants to start
    /// fresh; restart-the-server semantics already imply that).
    egressNames: string[];
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
    aboutOpen: boolean;
    accessOpen: boolean;
    /// Compare-with-previous picker modal — replaces the previous
    /// auto-pick-newest behaviour with a chooser when the user has
    /// more than one historical scan for the same username.
    comparePickerOpen: boolean;
    toast: { text: string; kind: "success" | "error" | "info" } | null;
    compareArmed: string | null;
    /// Set of site names currently being re-probed via the retry
    /// endpoint. The corresponding ResultRow shows a spinner state.
    retrying: Record<string, true>;
}

export interface AppStore {
    catalog: SiteSummary[];
    disabledCatalog: DisabledSiteSummary[];
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
    /// Non-null when the current route points at something missing.
    /// Mutually exclusive with `scan` / `diff` in practice.
    notFound: NotFoundState | null;
    /// True while a scan/diff is being fetched and there's nothing to
    /// show yet — drives the skeleton in the scan-view shell instead
    /// of an empty frame.
    loading: boolean;
    /// `adler-server` version from `GET /api/health`, shown in the
    /// footer. Null until the health probe resolves.
    serverVersion: string | null;
    /// Non-null while a multi-username batch is queued/running/just-
    /// finished. Independent of `scan` — each batch entry drives its
    /// own scan in turn, and the per-scan `clearScan` must not wipe it.
    batch: BatchState | null;
    history: ScanListEntry[];
    /// Access-engine config from `GET /api/access`. Loaded once at
    /// startup; the Access modal can refetch on demand. `null` until
    /// the bootstrap fetch resolves (or stays null if the endpoint
    /// errors — non-fatal).
    accessConfig: AccessResponse | null;
    view: ViewState;
    ui: UiState;
}

const persisted = loadPrefs();

const [store, set] = createStore<AppStore>({
    catalog: [],
    disabledCatalog: [],
    tagsBySite: {},
    categoryBySite: {},
    tagCounts: {},
    filter: {
        presetId: "quick",
        tag: [],
        excludeTag: ["bot-protected"],
        top: null,
        nsfw: persisted.nsfw ?? false,
        egressNames: [],
    },
    scan: null,
    diff: null,
    notFound: null,
    loading: false,
    serverVersion: null,
    batch: null,
    history: [],
    accessConfig: null,
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
        aboutOpen: false,
        accessOpen: false,
        comparePickerOpen: false,
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

const uiActions = createUiActions({ set, store });

export const actions = {
    ...createCatalogActions({ set, store }),
    ...createFilterActions({ set, store }),
    ...createScanActions({
        set,
        store,
        emptyBuckets,
        bucketsFrom,
        toast: uiActions.toast,
    }),
    ...createBatchActions({ set, store }),
    ...createDiffActions({ set }),
    ...createViewActions({ set, store }),
    ...uiActions,
};
