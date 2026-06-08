import { For, Show, createMemo, onMount, type Component } from "solid-js";

import { api } from "./api";
import { CATEGORIES, categoryForTags } from "./constants";
import { actions, store } from "./store";
import { displayUrl } from "./lib/format";
import type { CheckOutcome } from "./types";

import { About } from "./components/About";
import { AccessModal } from "./components/AccessModal";
import { AdvancedFilters } from "./components/AdvancedFilters";
import { BatchStrip } from "./components/BatchStrip";
import { ComparePicker } from "./components/ComparePicker";
import { DatacenterHint } from "./components/DatacenterHint";
import { Footer } from "./components/Footer";
import { Hero } from "./components/Hero";
import { HistoryDrawer } from "./components/HistoryDrawer";
import { IconSprite } from "./components/Icons";
import { NotFound } from "./components/NotFound";
import { ResultsList } from "./components/ResultsList";
import { ResultsToolbar } from "./components/ResultsToolbar";
import { ScanHeader } from "./components/ScanHeader";
import { ScanSkeleton } from "./components/ScanSkeleton";
import { ShortcutsOverlay } from "./components/ShortcutsOverlay";
import { Toast } from "./components/Toast";
import { TopBar } from "./components/TopBar";
import { useDocumentTitle } from "./hooks/useDocumentTitle";
import { useHashRouting } from "./hooks/useHashRouting";
import { useHistoryPolling } from "./hooks/useHistoryPolling";
import { useKeyboardShortcuts } from "./hooks/useKeyboardShortcuts";
import { useOutcomeBuffer } from "./hooks/useOutcomeBuffer";
import { useResultExport } from "./hooks/useResultExport";
import { useScanLifecycle } from "./hooks/useScanLifecycle";

export const App: Component = () => {
    const onOutcome = useOutcomeBuffer();
    const refreshHistory = useHistoryPolling();
    const lifecycle = useScanLifecycle(refreshHistory, onOutcome);
    const { urlHasView } = useHashRouting(lifecycle);
    const handleExport = useResultExport();
    useKeyboardShortcuts(lifecycle);

    /// Document title reflecting the current view. Read inside a
    /// `createEffect` so it tracks the relevant store slices.
    function computeTitle(): string {
        const base = "Adler";
        if (store.notFound) return `Not found — ${base}`;
        if (store.diff) {
            return `Diff: ${store.diff.a.username} ↔ ${store.diff.b.username} — ${base}`;
        }
        if (store.scan) {
            const s = store.scan;
            if (s.status === "running") return `Scanning ${s.username}… — ${base}`;
            return `${s.username} · ${s.summary?.found ?? 0} found — ${base}`;
        }
        if (store.loading) return `Loading… — ${base}`;
        return `${base} — OSINT username search`;
    }

    // ─────────── boot ───────────
    onMount(() => {
        // Fire data loads in parallel so the URL-routed scan doesn't
        // wait on the catalog fetch (catalog is only needed for chip
        // rendering / category bucketing — neither blocks the initial
        // scan-view paint).
        api.sites()
            .then(actions.setCatalog)
            .catch(() => actions.toast("Failed to load catalogue", "error"));
        api.access()
            .then((a) => actions.setAccessConfig(a))
            .catch(() => {
                // /api/access is a luxury, not a blocker — silent on fail.
            });
        // Footer version badge — best-effort; footer just omits it on failure.
        api.health()
            .then((h) => actions.setServerVersion(h.version))
            .catch(() => {});
    });

    useDocumentTitle(computeTitle);

    // ─────────── diff render ───────────
    const diffBreakdown = createMemo(() => {
        if (!store.diff) return null;
        const aFound = new Map(
            store.diff.a.outcomes.filter((o) => o.kind === "found").map((o) => [o.site, o]),
        );
        const bFound = new Map(
            store.diff.b.outcomes.filter((o) => o.kind === "found").map((o) => [o.site, o]),
        );
        const added: CheckOutcome[] = [];
        const removed: CheckOutcome[] = [];
        const kept: CheckOutcome[] = [];
        for (const [site, o] of bFound) (aFound.has(site) ? kept : added).push(o);
        for (const [site, o] of aFound) if (!bFound.has(site)) removed.push(o);
        return { added, removed, kept };
    });

    // ─────────── render ───────────
    // `hasView` is true if EITHER we already have state for the view
    // OR the URL points to one. The latter suppresses the Hero flash
    // on cold-load: scan-view shell renders immediately, then the
    // results stream in once `loadScan` resolves.
    const hasView = createMemo(
        () =>
            !!(store.scan || store.diff || store.loading || store.batch) ||
            urlHasView(),
    );
    void categoryForTags;
    void CATEGORIES;

    return (
        <>
            <IconSprite />
            <TopBar />
            <main>
                <Show
                    when={!store.notFound}
                    fallback={
                        <NotFound nf={store.notFound!} onHome={lifecycle.goHome} />
                    }
                >
                    <Show
                        when={hasView()}
                        fallback={
                            <Hero
                                onSubmit={(u) => {
                                    actions.clearBatch();
                                    lifecycle.startScan(u);
                                }}
                                onBatch={lifecycle.runBatch}
                            />
                        }
                    >
                        <section class="scan-view">
                            <Show when={store.batch}>
                                <BatchStrip
                                    onOpen={(id) => {
                                        location.hash = `#/scan/${id}`;
                                    }}
                                />
                            </Show>
                            <Show
                                when={store.scan || store.diff}
                                fallback={<ScanSkeleton />}
                            >
                                <ScanHeader
                                    onRescan={lifecycle.rescan}
                                    onStop={lifecycle.stopScan}
                                    onContinue={lifecycle.continueScan}
                                    onRestart={lifecycle.rescan}
                                    onExitDiff={lifecycle.exitDiff}
                                    onCompareWithPrevious={() => {
                                        // Hand off to ComparePicker; it
                                        // calls back through onPick below.
                                        actions.setComparePicker(true);
                                    }}
                                />
                                <Show
                                    when={store.scan && store.scan.status === "running"}
                                >
                                    <div class="progress-bar">
                                        <div
                                            class="fill"
                                            style={{
                                                width:
                                                    store.scan!.siteCount > 0
                                                        ? `${(store.scan!.outcomes.length / store.scan!.siteCount) * 100}%`
                                                        : "0%",
                                            }}
                                        />
                                    </div>
                                </Show>
                                <Show when={store.scan}>
                                    <ResultsToolbar onExport={handleExport} />
                                    <DatacenterHint />
                                    <ResultsList />
                                </Show>
                                <Show when={store.diff}>
                                    <DiffView
                                        added={diffBreakdown()!.added}
                                        removed={diffBreakdown()!.removed}
                                        kept={diffBreakdown()!.kept}
                                        a={store.diff!.a.username}
                                        b={store.diff!.b.username}
                                    />
                                </Show>
                            </Show>
                        </section>
                    </Show>
                </Show>
            </main>
            <Footer onAbout={() => actions.setAbout(true)} />

            <HistoryDrawer
                onOpenScan={lifecycle.openHistoryScan}
                onStartDiff={lifecycle.startDiff}
            />
            <AdvancedFilters onRefilter={lifecycle.refilterRunningScan} />
            <ShortcutsOverlay />
            <About />
            <AccessModal />
            <ComparePicker
                onPick={(prevId) => {
                    const cur = store.scan;
                    if (cur) lifecycle.startDiff(prevId, cur.id);
                }}
            />
            <Toast />
        </>
    );
};

const DiffView: Component<{
    added: CheckOutcome[];
    removed: CheckOutcome[];
    kept: CheckOutcome[];
    a: string;
    b: string;
}> = (p) => (
    <>
        <div class="category-head">
            <span class="name">Diff</span>
            <span class="stat">
                {p.a} → {p.b}
            </span>
            <span class="stat found">+{p.added.length}</span>
            <span class="stat" style={{ color: "var(--red)" }}>
                −{p.removed.length}
            </span>
            <span class="stat">={p.kept.length} unchanged</span>
        </div>
        <Show when={p.added.length > 0}>
            <div class="category-head">
                <span class="name" style={{ color: "var(--green-text)" }}>
                    + NEW
                </span>
                <span class="stat">{p.added.length}</span>
            </div>
            <For each={p.added}>
                {(o) => (
                    <div
                        class="result-row found"
                        style={{ "border-left": "2px solid var(--green)" }}
                    >
                        <div class="dot" />
                        <div class="site">
                            <span class="site-name">{o.site}</span>
                        </div>
                        <div class="url-cell">
                            <a href={displayUrl(o.url)} target="_blank" rel="noopener">
                                {displayUrl(o.url)}
                            </a>
                        </div>
                        <div class="meta-cell">{o.elapsed_ms}ms</div>
                    </div>
                )}
            </For>
        </Show>
        <Show when={p.removed.length > 0}>
            <div class="category-head">
                <span class="name" style={{ color: "var(--red)" }}>
                    − GONE
                </span>
                <span class="stat">{p.removed.length}</span>
            </div>
            <For each={p.removed}>
                {(o) => (
                    <div
                        class="result-row"
                        style={{
                            "border-left": "2px solid var(--red)",
                            opacity: 0.55,
                        }}
                    >
                        <div class="dot" />
                        <div class="site">
                            <span class="site-name">{o.site}</span>
                        </div>
                        <div class="url-cell">
                            <a href={displayUrl(o.url)} target="_blank" rel="noopener">
                                {displayUrl(o.url)}
                            </a>
                        </div>
                        <div class="meta-cell">{o.elapsed_ms}ms</div>
                    </div>
                )}
            </For>
        </Show>
        <Show when={p.added.length === 0 && p.removed.length === 0}>
            <div class="empty-results">No differences in found accounts</div>
        </Show>
    </>
);
