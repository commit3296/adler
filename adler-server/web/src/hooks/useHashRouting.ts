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

export function useHashRouting(lifecycle: ScanLifecycle) {
    const [routeHash, setRouteHash] = createSignal(location.hash);
    const urlHasView = createMemo(() => routeHasScanView(routeHash()));

    function handleHash() {
        setRouteHash(location.hash);
        const scanId = scanIdFromHash(location.hash);
        if (scanId) {
            lifecycle.loadScan(scanId);
            return;
        }
        const diffIds = diffIdsFromHash(location.hash);
        if (diffIds) {
            const cur = store.diff;
            if (cur && cur.a.id === diffIds[0] && cur.b.id === diffIds[1]) return;
            lifecycle.startDiff(diffIds[0], diffIds[1], { fromUrl: true });
            return;
        }
        lifecycle.closeStream();
        lifecycle.stopElapsedTimer();
        if (isHomeHash(location.hash)) {
            actions.clearBatch();
            actions.clearScan();
        } else {
            actions.setNotFound({ kind: "route", detail: location.hash });
        }
    }

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
