import { describe, expect, it } from "vitest";

import { createBatchActions } from "./batchActions";
import { createTestStore } from "./testHelpers";

describe("batchActions", () => {
    it("tracks queued, patched, finished, and cleared batch state", () => {
        const [store, set] = createTestStore();
        const actions = createBatchActions({ set, store });

        actions.startBatch(["alice", "bob"]);
        expect(store.batch).toEqual({
            entries: [
                { username: "alice", scanId: null, status: "queued", found: null },
                { username: "bob", scanId: null, status: "queued", found: null },
            ],
            running: true,
        });

        actions.updateBatchEntry(1, {
            scanId: "scan-bob",
            status: "done",
            found: 3,
        });
        expect(store.batch?.entries[1]).toEqual({
            username: "bob",
            scanId: "scan-bob",
            status: "done",
            found: 3,
        });

        actions.finishBatch();
        expect(store.batch?.running).toBe(false);

        actions.clearBatch();
        expect(store.batch).toBeNull();
    });
});
