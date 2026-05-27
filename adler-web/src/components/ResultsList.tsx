import { For, Show, createMemo, type Component } from "solid-js";
import { CATEGORIES, type Category } from "../constants";
import { actions, store } from "../store";
import type { CheckOutcome, MatchKind } from "../types";
import { ResultRow } from "./ResultRow";

function statusOrder(k: MatchKind): number {
    return k === "found" ? 0 : k === "uncertain" ? 1 : 2;
}

/// Common filter + sort applied within a bucket. Kept verbatim
/// between flat and grouped views so the user gets identical
/// ordering whether they pick group=Off or group=Category.
function applyFilterAndSort(
    bucket: CheckOutcome[],
    query: string,
    showNotFound: boolean,
    sort: "status" | "name" | "time",
): CheckOutcome[] {
    const q = query.trim().toLowerCase();
    let rows = bucket.filter((o) => {
        if (!showNotFound && o.kind === "not_found") return false;
        if (!q) return true;
        if (o.site.toLowerCase().includes(q)) return true;
        if (o.url.toLowerCase().includes(q)) return true;
        const tags = store.tagsBySite[o.site] ?? [];
        return tags.some((t) => t.toLowerCase().includes(q));
    });
    if (rows.length > 1) {
        rows = rows.slice();
        if (sort === "name") rows.sort((a, b) => a.site.localeCompare(b.site));
        else if (sort === "time") rows.sort((a, b) => b.elapsed_ms - a.elapsed_ms);
        else
            rows.sort((a, b) => {
                const c = statusOrder(a.kind) - statusOrder(b.kind);
                return c !== 0 ? c : a.site.localeCompare(b.site);
            });
    }
    return rows;
}

export const ResultsList: Component = () => {
    const nfCount = createMemo(() =>
        store.scan ? store.scan.outcomes.filter((o) => o.kind === "not_found").length : 0,
    );

    const emptyReason = createMemo(() => {
        if (!store.scan) return "Loading scan…";
        if (store.view.resultsFilter)
            return `Nothing matches "${store.view.resultsFilter}"`;
        if (store.scan.status === "finished") {
            if (store.scan.outcomes.length > 0)
                return `No accounts found for ${store.scan.username}. ${nfCount().toLocaleString()} sites checked.`;
            return "No sites scanned.";
        }
        return "Waiting for first result…";
    });

    /// Did we receive at least one displayable outcome anywhere across
    /// all groupings + the current filter? Used to decide between
    /// "show the list" and "show the empty-state copy".
    const hasDisplayable = createMemo(() => {
        if (!store.scan) return false;
        const q = store.view.resultsFilter.trim().toLowerCase();
        const showNF = store.view.showNotFound;
        for (const o of store.scan.outcomes) {
            if (!showNF && o.kind === "not_found") continue;
            if (!q) return true;
            if (o.site.toLowerCase().includes(q)) return true;
            if (o.url.toLowerCase().includes(q)) return true;
            const tags = store.tagsBySite[o.site] ?? [];
            if (tags.some((t) => t.toLowerCase().includes(q))) return true;
        }
        return false;
    });

    return (
        <>
            <Show when={store.scan && !hasDisplayable()}>
                <div class="empty-results">
                    <strong>{emptyReason()}</strong>
                </div>
            </Show>
            <Show when={store.scan && hasDisplayable()}>
                <Show
                    when={store.view.groupBy === "category"}
                    fallback={<FlatList />}
                >
                    {/* Outer For is over the static CATEGORIES list. Solid
                       never recreates the per-category DOM here — only
                       the matching CategoryBlock's inner memo re-runs
                       when its store-bucket changes. */}
                    <For each={CATEGORIES}>
                        {(cat) => <CategoryBlock cat={cat} />}
                    </For>
                </Show>
            </Show>
            <Show when={store.scan && !store.view.showNotFound && nfCount() > 0}>
                <div class="nf-footer">
                    <a onClick={() => actions.toggleShowNotFound()}>
                        {nfCount().toLocaleString()} not_found hidden — show
                    </a>
                </div>
            </Show>
            <Show when={store.scan && store.view.showNotFound && nfCount() > 0}>
                <div class="nf-footer">
                    <a onClick={() => actions.toggleShowNotFound()}>
                        Hide {nfCount().toLocaleString()} not_found
                    </a>
                </div>
            </Show>
        </>
    );
};

/// Flat view: applies filter + sort to the full outcomes array. Used
/// only when the user picks group=Off; the default grouped view uses
/// per-category buckets that re-evaluate independently.
const FlatList: Component = () => {
    const rows = createMemo(() =>
        applyFilterAndSort(
            store.scan?.outcomes ?? [],
            store.view.resultsFilter,
            store.view.showNotFound,
            store.view.sort,
        ),
    );
    return <For each={rows()}>{(o) => <ResultRow outcome={o} />}</For>;
};

const CategoryBlock: Component<{ cat: Category }> = (p) => {
    /// Reads the pre-bucketized array from the store. Solid's
    /// fine-grained reactivity ensures this memo only re-runs when
    /// THIS category's bucket array changes — appendOutcomes pushing
    /// into a different bucket doesn't trigger us. Sort/filter cost
    /// is on the bucket only (~315 items max, not 1890).
    const rows = createMemo(() =>
        applyFilterAndSort(
            store.scan?.bucketsByCategory?.[p.cat.id] ?? [],
            store.view.resultsFilter,
            store.view.showNotFound,
            store.view.sort,
        ),
    );
    const foundCount = createMemo(() => rows().filter((r) => r.kind === "found").length);
    const uncertainCount = createMemo(
        () => rows().filter((r) => r.kind === "uncertain").length,
    );
    return (
        <Show when={rows().length > 0}>
            <div class="category-group">
                <div class="category-head">
                    <span class="name">{p.cat.label}</span>
                    <span class="stat">
                        {rows().length} site{rows().length === 1 ? "" : "s"}
                    </span>
                    <Show when={foundCount() > 0}>
                        <span class="stat found">{foundCount()} found</span>
                    </Show>
                    <Show when={uncertainCount() > 0}>
                        <span class="stat uncertain">{uncertainCount()} uncertain</span>
                    </Show>
                </div>
                <For each={rows()}>{(o) => <ResultRow outcome={o} />}</For>
            </div>
        </Show>
    );
};
