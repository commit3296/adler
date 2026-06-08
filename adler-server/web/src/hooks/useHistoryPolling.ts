import { onCleanup, onMount } from "solid-js";

import { api } from "../api";
import { actions } from "../store";

export function useHistoryPolling(intervalMs = 8000): () => Promise<void> {
    let timer: number | null = null;

    async function refreshHistory() {
        try {
            const h = await api.scans();
            actions.setHistory(h);
        } catch {
            /* swallow — UI just shows the last good list */
        }
    }

    onMount(() => {
        refreshHistory();
        timer = window.setInterval(refreshHistory, intervalMs);
    });

    onCleanup(() => {
        if (timer !== null) window.clearInterval(timer);
    });

    return refreshHistory;
}
