import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { api } from "../api";
import type { CheckOutcome } from "../types";
import { createScanActions } from "./scanActions";
import { createTestStore } from "./testHelpers";

const filterAtStart = {
    tag: ["dev"],
    excludeTag: ["bot-protected"],
    top: 10,
    nsfw: false,
    egressNames: ["corp-de"],
};

const github: CheckOutcome = {
    site: "GitHub",
    url: "https://github.com/alice",
    kind: "found",
    elapsed_ms: 12,
};

const gitlab: CheckOutcome = {
    site: "GitLab",
    url: "https://gitlab.com/alice",
    kind: "not_found",
    elapsed_ms: 20,
};

function createActions() {
    const [store, set] = createTestStore({
        categoryBySite: {
            GitHub: "dev",
            GitLab: "dev",
            Mastodon: "social",
        },
    });
    const toast = vi.fn();
    const actions = createScanActions({
        set,
        store,
        emptyBuckets: () => ({ dev: [], social: [], other: [] }),
        bucketsFrom: (outcomes, categoryBySite) => {
            const buckets: Record<string, CheckOutcome[]> = {
                dev: [],
                social: [],
                other: [],
            };
            for (const outcome of outcomes) {
                const bucket = categoryBySite[outcome.site] ?? "other";
                buckets[bucket]!.push(outcome);
            }
            return buckets;
        },
        toast,
    });
    return { actions, store, toast };
}

describe("scanActions", () => {
    beforeEach(() => {
        vi.useFakeTimers();
        vi.setSystemTime(new Date("2026-06-08T12:00:00Z"));
    });

    afterEach(() => {
        vi.useRealTimers();
        vi.restoreAllMocks();
    });

    it("starts a scan and deduplicates streamed outcomes into buckets", () => {
        const { actions, store } = createActions();

        actions.beginScan("scan-1", "alice", 2, filterAtStart);
        actions.appendOutcome(github);
        actions.appendOutcome(github);
        actions.appendOutcomes([gitlab, github]);

        expect(store.scan?.status).toBe("running");
        expect(store.scan?.username).toBe("alice");
        expect(store.scan?.outcomes.map((o) => o.site)).toEqual([
            "GitHub",
            "GitLab",
        ]);
        expect(Object.keys(store.scan?.outcomeSites ?? {})).toEqual([
            "GitHub",
            "GitLab",
        ]);
        expect(store.scan?.bucketsByCategory.dev.map((o) => o.site)).toEqual([
            "GitHub",
            "GitLab",
        ]);
    });

    it("rebinding after refilter keeps identity fresh and clears buffers", () => {
        const { actions, store } = createActions();

        actions.beginScan("scan-1", "alice", 2, filterAtStart);
        actions.appendOutcome(github);
        actions.rebindScanAfterRefilter("scan-2", 1, {
            ...filterAtStart,
            tag: ["social"],
        });

        expect(store.scan?.id).toBe("scan-2");
        expect(store.scan?.siteCount).toBe(1);
        expect(store.scan?.outcomes).toEqual([]);
        expect(store.scan?.outcomeSites).toEqual({});
        expect(store.scan?.summary).toBeNull();
        expect(store.scan?.filterAtStart.tag).toEqual(["social"]);
    });

    it("finishes and loads scans with derived lookup state", () => {
        const { actions, store } = createActions();

        actions.beginScan("scan-1", "alice", 2, filterAtStart);
        actions.finishScan(
            { found: 1, not_found: 1, uncertain: 0 },
            [github, gitlab],
            64,
        );

        expect(store.scan?.status).toBe("finished");
        expect(store.scan?.summary).toEqual({
            found: 1,
            not_found: 1,
            uncertain: 0,
        });
        expect(store.scan?.elapsedMs).toBe(64);
        expect(store.scan?.bucketsByCategory.dev).toHaveLength(2);

        actions.loadScan({
            id: "old-scan",
            username: "bob",
            filterAtStart,
            outcomes: [github],
            outcomeSites: {},
            bucketsByCategory: {},
            status: "finished",
            summary: { found: 1, not_found: 0, uncertain: 0 },
            siteCount: 1,
            startedAtMs: Date.now() - 10,
            elapsedMs: 10,
        });

        expect(store.scan?.id).toBe("old-scan");
        expect(store.scan?.outcomeSites).toEqual({ GitHub: true });
        expect(store.scan?.bucketsByCategory.dev.map((o) => o.site)).toEqual([
            "GitHub",
        ]);
    });

    it("retries a site, updates summary, and clears retrying state", async () => {
        const { actions, store, toast } = createActions();
        vi.spyOn(api, "retrySite").mockResolvedValue({
            outcome: { ...gitlab, kind: "found" },
        });

        actions.beginScan("scan-1", "alice", 2, filterAtStart);
        actions.appendOutcomes([github, gitlab]);
        await actions.retrySite("GitLab");

        expect(api.retrySite).toHaveBeenCalledWith("scan-1", "GitLab");
        expect(store.ui.retrying).toEqual({});
        expect(store.scan?.outcomes.find((o) => o.site === "GitLab")?.kind).toBe(
            "found",
        );
        expect(store.scan?.summary).toEqual({
            found: 2,
            not_found: 0,
            uncertain: 0,
        });
        expect(toast).toHaveBeenCalledWith("GitLab: found", "success");
    });
});
