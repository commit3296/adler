import { createSignal, onCleanup } from "solid-js";

import { ApiClientError, api, streamScan } from "../api";
import { filterSnapshot, refilterRequestBody, scanRequestBody } from "../lib/scanRequest";
import { actions, store } from "../store";

type RefreshHistory = () => Promise<void>;
type OutcomeHandler = (outcome: unknown) => void;

export function useScanLifecycle(
    refreshHistory: RefreshHistory,
    onOutcome: OutcomeHandler,
) {
    let sseClose: (() => void) | null = null;
    let elapsedTimer: number | null = null;
    const [lastUsername, setLastUsername] = createSignal<string>("");

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

    function startElapsedTimer() {
        elapsedTimer = window.setInterval(() => actions.tickElapsed(), 100);
    }

    function currentFilterSnapshot() {
        return filterSnapshot(store.filter);
    }

    async function startScan(username: string): Promise<string | null> {
        closeStream();
        stopElapsedTimer();
        actions.clearScan();
        setLastUsername(username);
        actions.setLoading(true);

        try {
            const filterAtStart = currentFilterSnapshot();
            const r = await api.startScan(scanRequestBody(username, store.filter));
            actions.beginScan(r.scan_id, r.username, r.site_count, filterAtStart);
            history.replaceState(null, "", `#/scan/${r.scan_id}`);
            startElapsedTimer();
            return await new Promise<string | null>((resolve) => {
                sseClose = streamScan(r.scan_id, {
                    onOutcome,
                    onDone: (f) => {
                        stopElapsedTimer();
                        actions.finishScan(f.summary, f.outcomes, f.elapsed_ms);
                        refreshHistory();
                        resolve(r.scan_id);
                    },
                    onError: () => {
                        actions.toast("Stream disconnected", "error");
                        stopElapsedTimer();
                        resolve(null);
                    },
                });
                refreshHistory();
            });
        } catch (err) {
            const msg = apiErrorMessage(err);
            actions.toast(`Scan failed: ${msg}`, "error");
            actions.setLoading(false);
            stopElapsedTimer();
            return null;
        }
    }

    async function runBatch(usernames: string[]) {
        const uniq = [...new Set(usernames.map((u) => u.trim()).filter(Boolean))];
        if (uniq.length === 0) return;
        if (uniq.length === 1) {
            actions.clearBatch();
            await startScan(uniq[0]!);
            return;
        }
        actions.startBatch(uniq);
        for (let i = 0; i < uniq.length; i++) {
            if (!store.batch) break;
            actions.updateBatchEntry(i, { status: "running" });
            const id = await startScan(uniq[i]!);
            if (!store.batch) break;
            if (id) {
                const found =
                    store.scan?.id === id ? (store.scan.summary?.found ?? 0) : 0;
                actions.updateBatchEntry(i, { status: "done", scanId: id, found });
            } else {
                actions.updateBatchEntry(i, { status: "error" });
            }
        }
        actions.finishBatch();
    }

    async function rescan() {
        if (store.scan) await startScan(store.scan.username);
        else if (lastUsername()) await startScan(lastUsername());
    }

    async function refilterRunningScan() {
        const cur = store.scan;
        if (!cur || cur.status !== "running") return;
        try {
            const filterAtStart = currentFilterSnapshot();
            const r = await api.refilterScan(cur.id, refilterRequestBody(store.filter));
            closeStream();
            stopElapsedTimer();
            actions.rebindScanAfterRefilter(r.scan_id, r.site_count, filterAtStart);
            history.replaceState(null, "", `#/scan/${r.scan_id}`);
            startElapsedTimer();
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
            actions.toast(
                r.carried_outcomes > 0
                    ? `Refiltered — ${r.carried_outcomes} carried over, ${r.site_count - r.carried_outcomes} to probe`
                    : `Refiltered — ${r.site_count} sites to probe`,
                "success",
            );
            refreshHistory();
        } catch (err) {
            const msg = apiErrorMessage(err);
            actions.toast(`Refilter failed: ${msg}`, "error");
        }
    }

    function stopScan() {
        closeStream();
        stopElapsedTimer();
        actions.pauseScan();
        actions.toast("Stopped — backend keeps probing in the background", "info");
    }

    function continueScan() {
        if (!store.scan) return;
        const id = store.scan.id;
        actions.resumeScan();
        startElapsedTimer();
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
        actions.setLoading(true);
        try {
            const data = await api.scan(id);
            if (data.status === "finished") {
                actions.loadScan({
                    id,
                    username: data.username,
                    filterAtStart: currentFilterSnapshot(),
                    outcomes: data.outcomes,
                    outcomeSites: {},
                    bucketsByCategory: {},
                    status: "finished",
                    summary: data.summary,
                    siteCount: data.site_count,
                    startedAtMs: Date.now() - (data.elapsed_ms || 0),
                    elapsedMs: data.elapsed_ms,
                });
                setLastUsername(data.username);
            } else {
                actions.beginScan(
                    id,
                    data.username,
                    data.site_count,
                    currentFilterSnapshot(),
                );
                setLastUsername(data.username);
                startElapsedTimer();
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
            if (err instanceof ApiClientError && err.code === "scan_not_found") {
                actions.setNotFound({ kind: "scan", detail: id });
            } else {
                const msg = err instanceof ApiClientError ? err.message : String(err);
                actions.toast(`Failed to load scan: ${msg}`, "error");
                actions.setLoading(false);
            }
        }
    }

    async function startDiff(
        aId: string,
        bId: string,
        opts: { fromUrl?: boolean } = {},
    ) {
        actions.setLoading(true);
        try {
            const [scanDiff, a, b] = await Promise.all([
                api.scanDiff(aId, bId),
                api.scan(aId),
                api.scan(bId),
            ]);
            const outA = a.status === "finished" ? a.outcomes : a.partial;
            const outB = b.status === "finished" ? b.outcomes : b.partial;
            closeStream();
            stopElapsedTimer();
            actions.setDiff({
                a: { id: aId, username: a.username, outcomes: outA },
                b: { id: bId, username: b.username, outcomes: outB },
                scanDiff,
            });
            actions.setDrawer(false);
            if (!opts.fromUrl) location.hash = `#/diff/${aId}/${bId}`;
        } catch (err) {
            if (err instanceof ApiClientError && err.code === "scan_not_found") {
                actions.setNotFound({ kind: "diff", detail: `${aId} / ${bId}` });
            } else {
                const msg = err instanceof ApiClientError ? err.message : String(err);
                actions.toast(`Diff failed: ${msg}`, "error");
                actions.setLoading(false);
                if (opts.fromUrl) history.replaceState(null, "", "#/");
            }
        }
    }

    function exitDiff() {
        const d = store.diff;
        if (window.history.length > 1) {
            window.history.back();
            return;
        }
        location.hash = d ? `#/scan/${d.b.id}` : "#/";
    }

    function goHome() {
        actions.clearBatch();
        actions.clearScan();
        location.hash = "#/";
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

    onCleanup(() => {
        closeStream();
        stopElapsedTimer();
    });

    return {
        closeStream,
        continueScan,
        exitDiff,
        goHome,
        loadScan,
        openHistoryScan,
        refilterRunningScan,
        rescan,
        runBatch,
        startDiff,
        startScan,
        stopElapsedTimer,
        stopScan,
        isStreaming: () => sseClose !== null,
    };
}

function apiErrorMessage(err: unknown): string {
    if (!(err instanceof ApiClientError)) return String(err);
    if (err.disabledMatches.length === 0) return err.message;
    const first = err.disabledMatches[0]!;
    const suffix =
        err.disabledMatches.length > 1
            ? ` and ${err.disabledMatches.length - 1} more`
            : "";
    return `${err.message}: ${first.name} is parked (${first.disabled_reason})${suffix}`;
}
