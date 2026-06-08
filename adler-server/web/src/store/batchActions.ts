import type { SetStoreFunction } from "solid-js/store";

import type { AppStore, BatchEntry } from "../store";

interface Deps {
    set: SetStoreFunction<AppStore>;
    store: AppStore;
}

export function createBatchActions({ set, store }: Deps) {
    return {
        startBatch(usernames: string[]) {
            set("batch", {
                entries: usernames.map((u) => ({
                    username: u,
                    scanId: null,
                    status: "queued" as const,
                    found: null,
                })),
                running: true,
            });
        },
        updateBatchEntry(index: number, patch: Partial<BatchEntry>) {
            if (!store.batch) return;
            set("batch", "entries", index, (e) => ({ ...e, ...patch }));
        },
        finishBatch() {
            if (!store.batch) return;
            set("batch", "running", false);
        },
        clearBatch() {
            set("batch", null);
        },
    };
}
