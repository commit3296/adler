import { createMemo, createSignal, onCleanup, onMount } from "solid-js";

import {
    diffIdsFromHash,
    isHomeHash,
    routeHasScanView,
    scanIdFromHash,
} from "../lib/routes";
import { actions, store } from "../store";
import type { useScanLifecycle } from "./useScanLifecycle";

type ScanLifecycle = ReturnType<typeof useScanLifecycle>;

interface HashRoutingDeps {
    actions: Pick<typeof actions, "clearBatch" | "clearScan" | "setNotFound">;
    getHash: () => string;
    lifecycle: Pick<
        ScanLifecycle,
        "closeStream" | "loadScan" | "startDiff" | "stopElapsedTimer"
    >;
    setRouteHash: (hash: string) => void;
    store: Pick<typeof store, "diff">;
}

export function createHashRouteHandler({
    actions,
    getHash,
    lifecycle,
    setRouteHash,
    store,
}: HashRoutingDeps): () => void {
    return () => {
        const hash = getHash();
        setRouteHash(hash);
        const scanId = scanIdFromHash(hash);
        if (scanId) {
            lifecycle.loadScan(scanId);
            return;
        }
        const diffIds = diffIdsFromHash(hash);
        if (diffIds) {
            const cur = store.diff;
            if (cur && cur.a.id === diffIds[0] && cur.b.id === diffIds[1]) return;
            lifecycle.startDiff(diffIds[0], diffIds[1], { fromUrl: true });
            return;
        }
        lifecycle.closeStream();
        lifecycle.stopElapsedTimer();
        if (isHomeHash(hash)) {
            actions.clearBatch();
            actions.clearScan();
        } else {
            actions.setNotFound({ kind: "route", detail: hash });
        }
    };
}

export function useHashRouting(lifecycle: ScanLifecycle) {
    const [routeHash, setRouteHash] = createSignal(location.hash);
    const urlHasView = createMemo(() => routeHasScanView(routeHash()));
    const handleHash = createHashRouteHandler({
        actions,
        getHash: () => location.hash,
        lifecycle,
        setRouteHash,
        store,
    });

    onMount(() => {
        window.addEventListener("hashchange", handleHash);

        const initScan = scanIdFromHash(location.hash);
        const initDiff = diffIdsFromHash(location.hash);
        if (initScan) lifecycle.loadScan(initScan);
        else if (initDiff)
            lifecycle.startDiff(initDiff[0], initDiff[1], { fromUrl: true });
        else if (!isHomeHash(location.hash))
            actions.setNotFound({ kind: "route", detail: location.hash });
    });

    onCleanup(() => {
        window.removeEventListener("hashchange", handleHash);
    });

    return { urlHasView };
}
