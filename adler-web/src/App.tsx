import {
    For,
    Show,
    createMemo,
    createSignal,
    onCleanup,
    onMount,
    type Component,
} from "solid-js";

/// Match the URL patterns that should render the scan-view shell
/// (even before the actual data has loaded). Used to suppress the
/// Hero flash on cold-load of `#/scan/xxx` or `#/diff/a/b`.
const ROUTE_VIEW_RE = /^#\/(scan|diff)\//;
import { ApiClientError, api, streamScan } from "./api";
import { CATEGORIES, categoryForTags } from "./constants";
import { actions, store } from "./store";
import { displayUrl } from "./lib/format";
import type { CheckOutcome } from "./types";

import { AdvancedFilters } from "./components/AdvancedFilters";
import { DatacenterHint } from "./components/DatacenterHint";
import { Hero } from "./components/Hero";
import { HistoryDrawer } from "./components/HistoryDrawer";
import { IconSprite } from "./components/Icons";
import { ResultsList } from "./components/ResultsList";
import { ResultsToolbar } from "./components/ResultsToolbar";
import { ScanHeader } from "./components/ScanHeader";
import { ShortcutsOverlay } from "./components/ShortcutsOverlay";
import { Toast } from "./components/Toast";
import { TopBar } from "./components/TopBar";

export const App: Component = () => {
    // ─────────── transient (non-store) state ───────────
    let sseClose: (() => void) | null = null;
    let elapsedTimer: number | null = null;
    let historyTimer: number | null = null;
    const [lastUsername, setLastUsername] = createSignal<string>("");

    // ─────────── helpers ───────────
    function closeStream() {
        if (sseClose) {
            sseClose();
            sseClose = null;
        }
    }
    function stopElapsedTimer() {
        if (elapsedTimer !== null) {
            window.clearInterval(elapsedTimer);
            elapsedTimer = null;
        }
    }

    /// Buffer incoming SSE `outcome` events for one animation frame
    /// before applying them to the store. The server fires bursts
    /// (subscribe-time replay, plus naturally clustered probes from
    /// the executor's 32-way concurrency), and applying each one as a
    /// separate store mutation made every downstream memo
    /// (filter / sort / per-category bucket / `<For>` reconciliation)
    /// re-evaluate per event. With one batched apply per rAF, the
    /// whole pipeline runs at most 60 ×/sec regardless of arrival rate.
    // Reactive copy of `location.hash` so render-time route checks
    // (e.g. "should we show the scan-view shell yet?") update on
    // hashchange without polling.
    const [routeHash, setRouteHash] = createSignal(location.hash);
    const urlHasView = createMemo(() => ROUTE_VIEW_RE.test(routeHash()));

    let outcomeBuffer: ReturnType<typeof Object>[] = [];
    let outcomeRafQueued = false;
    function onOutcome(o: unknown) {
        outcomeBuffer.push(o);
        if (outcomeRafQueued) return;
        outcomeRafQueued = true;
        requestAnimationFrame(() => {
            const batch = outcomeBuffer as Parameters<typeof actions.appendOutcomes>[0];
            outcomeBuffer = [];
            outcomeRafQueued = false;
            actions.appendOutcomes(batch);
        });
    }

    // ─────────── scan lifecycle ───────────
    async function startScan(username: string) {
        closeStream();
        stopElapsedTimer();
        actions.clearScan();
        setLastUsername(username);

        const body: any = { username };
        if (store.filter.tag.length) body.tag = store.filter.tag;
        if (store.filter.excludeTag.length) body.exclude_tag = store.filter.excludeTag;
        if (store.filter.top != null) body.top = store.filter.top;
        if (store.filter.nsfw) body.nsfw = true;

        try {
            const r = await api.startScan(body);
            actions.beginScan(r.scan_id, r.username, r.site_count);
            history.replaceState(null, "", `#/scan/${r.scan_id}`);

            elapsedTimer = window.setInterval(() => actions.tickElapsed(), 100);
            sseClose = streamScan(r.scan_id, {
                onOutcome,
                onDone: (f) => {
                    stopElapsedTimer();
                    actions.finishScan(f.summary, f.outcomes, f.elapsed_ms);
                    refreshHistory();
                },
                onError: () => {
                    actions.toast("Stream disconnected", "error");
                    stopElapsedTimer();
                },
            });
            refreshHistory();
        } catch (err) {
            const msg = err instanceof ApiClientError ? err.message : String(err);
            actions.toast(`Scan failed: ${msg}`, "error");
            stopElapsedTimer();
        }
    }

    async function rescan() {
        if (store.scan) await startScan(store.scan.username);
        else if (lastUsername()) await startScan(lastUsername());
    }

    function stopScan() {
        closeStream();
        stopElapsedTimer();
        actions.pauseScan();
        actions.toast(
            "Stopped — backend keeps probing in the background",
            "info",
        );
    }

    /// Re-open the SSE stream for the current scan. Useful after Stop:
    /// the backend is probably still running and will pick up streaming
    /// where it left off (the server replays its history on connect).
    function continueScan() {
        if (!store.scan) return;
        const id = store.scan.id;
        actions.resumeScan();
        elapsedTimer = window.setInterval(() => actions.tickElapsed(), 100);
        sseClose = streamScan(id, {
            onOutcome: (o) => actions.appendOutcome(o),
            onDone: (f) => {
                stopElapsedTimer();
                actions.finishScan(f.summary, f.outcomes, f.elapsed_ms);
                refreshHistory();
            },
            onError: () => {
                actions.toast("Stream disconnected", "error");
                stopElapsedTimer();
            },
        });
    }

    async function loadScan(id: string) {
        if (store.scan?.id === id && !store.diff) return;
        closeStream();
        stopElapsedTimer();
        try {
            const data = await api.scan(id);
            if (data.status === "finished") {
                actions.loadScan({
                    id,
                    username: data.username,
                    outcomes: data.outcomes,
                    outcomeSites: {}, // backfilled inside loadScan
                    bucketsByCategory: {}, // backfilled inside loadScan
                    status: "finished",
                    summary: data.summary,
                    siteCount: data.site_count,
                    startedAtMs: Date.now() - (data.elapsed_ms || 0),
                    elapsedMs: data.elapsed_ms,
                });
                setLastUsername(data.username);
            } else {
                actions.beginScan(id, data.username, data.site_count);
                setLastUsername(data.username);
                elapsedTimer = window.setInterval(() => actions.tickElapsed(), 100);
                sseClose = streamScan(id, {
                    onOutcome,
                    onDone: (f) => {
                        stopElapsedTimer();
                        actions.finishScan(f.summary, f.outcomes, f.elapsed_ms);
                        refreshHistory();
                    },
                    onError: () => {
                        actions.toast("Stream disconnected", "error");
                        stopElapsedTimer();
                    },
                });
            }
        } catch (err) {
            const msg = err instanceof ApiClientError ? err.message : String(err);
            actions.toast(`Failed to load scan: ${msg}`, "error");
        }
    }

    /// Open the diff view comparing two scans. When invoked by the
    /// user (not by hashchange), pushes `#/diff/a/b` onto browser
    /// history so the native back button can return to the previous
    /// scan view.
    async function startDiff(
        aId: string,
        bId: string,
        opts: { fromUrl?: boolean } = {},
    ) {
        try {
            const [a, b] = await Promise.all([api.scan(aId), api.scan(bId)]);
            const outA = a.status === "finished" ? a.outcomes : a.partial;
            const outB = b.status === "finished" ? b.outcomes : b.partial;
            closeStream();
            stopElapsedTimer();
            actions.setDiff({
                a: { id: aId, username: a.username, outcomes: outA },
                b: { id: bId, username: b.username, outcomes: outB },
            });
            actions.setDrawer(false);
            if (!opts.fromUrl) {
                location.hash = `#/diff/${aId}/${bId}`;
            }
        } catch (err) {
            const msg = err instanceof ApiClientError ? err.message : String(err);
            actions.toast(`Diff failed: ${msg}`, "error");
            // Bail out of a broken diff URL — back to home.
            if (opts.fromUrl) history.replaceState(null, "", "#/");
        }
    }

    /// Leave the diff view. Uses native back when there's somewhere
    /// to go, otherwise falls back to the "current" scan (b) or home.
    function exitDiff() {
        const d = store.diff;
        if (window.history.length > 1) {
            window.history.back();
            return;
        }
        if (d) {
            location.hash = `#/scan/${d.b.id}`;
        } else {
            location.hash = "#/";
        }
    }

    function openHistoryScan(id: string) {
        if (store.ui.compareArmed) {
            startDiff(store.ui.compareArmed, id);
            actions.armCompare(null);
        } else {
            actions.setDrawer(false);
            location.hash = `#/scan/${id}`;
        }
    }

    // ─────────── history polling ───────────
    async function refreshHistory() {
        try {
            const h = await api.scans();
            actions.setHistory(h);
        } catch {
            /* swallow — UI just shows the last good list */
        }
    }

    // ─────────── boot ───────────
    onMount(() => {
        window.addEventListener("hashchange", handleHash);
        window.addEventListener("keydown", handleKey);

        // Fire data loads in parallel so the URL-routed scan doesn't
        // wait on the catalog fetch (catalog is only needed for chip
        // rendering / category bucketing — neither blocks the initial
        // scan-view paint).
        api.sites()
            .then(actions.setCatalog)
            .catch(() => actions.toast("Failed to load catalogue", "error"));
        refreshHistory();
        historyTimer = window.setInterval(refreshHistory, 8000);

        // Handle the initial route synchronously. `urlHasView()` is
        // already true at this point, so the scan-view shell renders
        // immediately; the real outcomes arrive moments later.
        const initScan = location.hash.match(/^#\/scan\/([a-z0-9]+)/);
        const initDiff = location.hash.match(/^#\/diff\/([a-z0-9]+)\/([a-z0-9]+)/);
        if (initScan) loadScan(initScan[1]!);
        else if (initDiff) startDiff(initDiff[1]!, initDiff[2]!, { fromUrl: true });
    });

    onCleanup(() => {
        closeStream();
        stopElapsedTimer();
        if (historyTimer !== null) window.clearInterval(historyTimer);
        window.removeEventListener("hashchange", handleHash);
        window.removeEventListener("keydown", handleKey);
    });

    function handleHash() {
        setRouteHash(location.hash);
        const sm = location.hash.match(/^#\/scan\/([a-z0-9]+)/);
        if (sm) {
            loadScan(sm[1]!);
            return;
        }
        const dm = location.hash.match(/^#\/diff\/([a-z0-9]+)\/([a-z0-9]+)/);
        if (dm) {
            // Don't restart if we're already showing this exact diff.
            const cur = store.diff;
            if (cur && cur.a.id === dm[1] && cur.b.id === dm[2]) return;
            startDiff(dm[1]!, dm[2]!, { fromUrl: true });
            return;
        }
        // Home / empty hash.
        closeStream();
        stopElapsedTimer();
        actions.clearScan();
    }

    // ─────────── keyboard ───────────
    function activeRows(): HTMLElement[] {
        return Array.from(document.querySelectorAll<HTMLElement>(".result-row"));
    }
    function moveSelection(delta: number) {
        const rows = activeRows();
        if (rows.length === 0) return;
        const sites = rows.map((r) => r.dataset.site!);
        let idx = store.view.selectedSite ? sites.indexOf(store.view.selectedSite) : -1;
        idx = Math.max(0, Math.min(rows.length - 1, idx + delta));
        // Visual outline applied directly — store-based would need a row prop
        for (const r of rows) r.style.outline = "";
        rows[idx]!.style.outline = "2px solid var(--red)";
        rows[idx]!.style.outlineOffset = "-1px";
        rows[idx]!.scrollIntoView({ block: "nearest" });
        actions.selectSite(sites[idx]!);
    }
    function selectedOutcome(): CheckOutcome | null {
        if (!store.scan || !store.view.selectedSite) return null;
        return store.scan.outcomes.find((o) => o.site === store.view.selectedSite) ?? null;
    }
    function handleKey(e: KeyboardEvent) {
        const tag = (e.target as HTMLElement | null)?.tagName?.toLowerCase() ?? "";
        const inField = tag === "input" || tag === "textarea";

        if (e.key === "Escape") {
            // Cascade: overlays → modal → drawer → running stream → diff view.
            // Esc means "back off one layer", never crosses two boundaries.
            if (store.ui.shortcutsOpen) {
                actions.setShortcuts(false);
                return;
            }
            if (store.ui.filtersOpen) {
                actions.setFilters(false);
                return;
            }
            if (store.ui.drawerOpen) {
                actions.setDrawer(false);
                return;
            }
            if (sseClose) {
                stopScan();
                return;
            }
            if (store.diff) {
                exitDiff();
                return;
            }
            return;
        }
        if (inField) return;

        if (e.key === "/") {
            e.preventDefault();
            const el = document.getElementById("username") as HTMLInputElement | null;
            if (el) {
                el.focus();
                el.select();
            }
            return;
        }
        if (e.key === "?") {
            e.preventDefault();
            actions.setShortcuts(true);
            return;
        }
        if (e.key === "h") {
            e.preventDefault();
            actions.setDrawer(!store.ui.drawerOpen);
            return;
        }
        if (e.key === "f") {
            e.preventDefault();
            actions.setFilters(true);
            return;
        }
        if (e.key === "n") {
            actions.toggleShowNotFound();
            return;
        }
        if (e.key === "j" || e.key === "ArrowDown") {
            e.preventDefault();
            moveSelection(1);
            return;
        }
        if (e.key === "k" || e.key === "ArrowUp") {
            e.preventDefault();
            moveSelection(-1);
            return;
        }
        if (e.key === "o") {
            const o = selectedOutcome();
            if (o) window.open(displayUrl(o.url), "_blank", "noopener");
            return;
        }
        if (e.key === "c") {
            const o = selectedOutcome();
            if (!o) return;
            navigator.clipboard
                .writeText(displayUrl(o.url))
                .then(() => actions.toast("URL copied", "success"))
                .catch(() => actions.toast("Copy blocked", "error"));
            return;
        }
    }

    // ─────────── export ───────────
    function handleExport(kind: "json" | "csv" | "urls") {
        if (!store.scan) return;
        if (kind === "urls") {
            const urls = store.scan.outcomes
                .filter((o) => o.kind === "found")
                .map((o) => displayUrl(o.url));
            if (urls.length === 0) {
                actions.toast("No found URLs to copy", "error");
                return;
            }
            navigator.clipboard
                .writeText(urls.join("\n"))
                .then(() => actions.toast(`Copied ${urls.length} URLs`, "success"))
                .catch(() => actions.toast("Copy failed", "error"));
            return;
        }
        const filename = `adler-${store.scan.username}-${store.scan.id}.${kind}`;
        let body = "";
        let type = "";
        if (kind === "json") {
            body = JSON.stringify(
                {
                    username: store.scan.username,
                    scan_id: store.scan.id,
                    summary: store.scan.summary,
                    outcomes: store.scan.outcomes,
                },
                null,
                2,
            );
            type = "application/json";
        } else {
            const lines = ["site,kind,url,elapsed_ms,reason"];
            const csvEscape = (v: string) =>
                /[",\n]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v;
            for (const o of store.scan.outcomes) {
                const reason =
                    o.reason === undefined
                        ? ""
                        : typeof o.reason === "string"
                          ? o.reason
                          : Object.keys(o.reason)[0]!;
                lines.push(
                    [
                        csvEscape(o.site),
                        csvEscape(o.kind),
                        csvEscape(o.url),
                        String(o.elapsed_ms),
                        csvEscape(reason),
                    ].join(","),
                );
            }
            body = lines.join("\n");
            type = "text/csv";
        }
        const blob = new Blob([body], { type });
        const a = document.createElement("a");
        a.href = URL.createObjectURL(blob);
        a.download = filename;
        document.body.appendChild(a);
        a.click();
        a.remove();
        setTimeout(() => URL.revokeObjectURL(a.href), 5000);
    }

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
    const hasView = createMemo(() => !!(store.scan || store.diff) || urlHasView());
    void categoryForTags;
    void CATEGORIES;

    return (
        <>
            <IconSprite />
            <TopBar />
            <main>
                <Show
                    when={hasView()}
                    fallback={<Hero onSubmit={(u) => startScan(u)} />}
                >
                    <section class="scan-view">
                        <ScanHeader
                            onRescan={rescan}
                            onStop={stopScan}
                            onContinue={continueScan}
                            onRestart={rescan}
                            onExitDiff={exitDiff}
                            onCompareWithPrevious={() => {
                                const cur = store.scan;
                                if (!cur) return;
                                const prev = store.history.find(
                                    (h) =>
                                        h.username === cur.username &&
                                        h.scan_id !== cur.id &&
                                        h.status === "finished",
                                );
                                if (!prev) return;
                                startDiff(prev.scan_id, cur.id);
                            }}
                        />
                        <Show when={store.scan && store.scan.status === "running"}>
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
                    </section>
                </Show>
            </main>

            <HistoryDrawer onOpenScan={openHistoryScan} onStartDiff={startDiff} />
            <AdvancedFilters />
            <ShortcutsOverlay />
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
