import { createRoot } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { FinishedScan, StartScanBody } from "../types";

const mocks = vi.hoisted(() => ({
    scan: vi.fn(),
    scanDiff: vi.fn(),
    startScan: vi.fn(),
    streamScan: vi.fn(),
}));

vi.mock("../api", async () => {
    const actual = await vi.importActual<typeof import("../api")>("../api");
    return {
        ...actual,
        api: {
            scan: mocks.scan,
            scanDiff: mocks.scanDiff,
            startScan: mocks.startScan,
        },
        streamScan: mocks.streamScan,
    };
});

import { api } from "../api";
import { actions, store } from "../store";
import { useScanLifecycle } from "./useScanLifecycle";

describe("useScanLifecycle", () => {
    beforeEach(() => {
        vi.useFakeTimers();
        vi.setSystemTime(new Date("2026-06-08T12:00:00Z"));
        vi.stubGlobal("history", { replaceState: vi.fn() });
        vi.stubGlobal("window", {
            clearInterval: vi.fn(),
            setInterval: vi.fn(() => 1),
        });
        actions.clearBatch();
        actions.clearScan();
        actions.setLoading(false);
        mocks.startScan.mockReset();
        mocks.scan.mockReset();
        mocks.scanDiff.mockReset();
        mocks.streamScan.mockReset();
    });

    afterEach(() => {
        vi.unstubAllGlobals();
        vi.useRealTimers();
        vi.restoreAllMocks();
    });

    it("starts a scan, binds the route, and finishes from SSE", async () => {
        const finished: FinishedScan = {
            summary: { found: 1, not_found: 0, uncertain: 0 },
            outcomes: [
                {
                    site: "GitHub",
                    url: "https://github.com/alice",
                    kind: "found",
                    elapsed_ms: 12,
                },
            ],
            elapsed_ms: 42,
            identity_clusters: [
                {
                    id: "identity-0001",
                    confidence: 90,
                    uncertain: false,
                    reasons: [
                        {
                            kind: "shared_external_link",
                            value: "https://alice.dev",
                        },
                    ],
                    members: [
                        {
                            site: "GitHub",
                            username: "alice",
                            url: "https://github.com/alice",
                            confidence: {
                                score: 85,
                                label: "high",
                                reasons: [],
                            },
                        },
                        {
                            site: "GitLab",
                            username: "alice",
                            url: "https://gitlab.com/alice",
                            confidence: {
                                score: 85,
                                label: "high",
                                reasons: [],
                            },
                        },
                    ],
                },
            ],
        };
        mocks.startScan.mockResolvedValue({
            scan_id: "scan1",
            username: "alice",
            site_count: 1,
        });
        mocks.streamScan.mockImplementation((_id, handlers) => {
            queueMicrotask(() => handlers.onDone(finished));
            return vi.fn();
        });
        const refreshHistory = vi.fn(() => Promise.resolve());
        const onOutcome = vi.fn();

        await createRoot(async (dispose) => {
            const lifecycle = useScanLifecycle(refreshHistory, onOutcome);
            const id = await lifecycle.startScan("alice");
            dispose();
            return id;
        });

        expect(api.startScan).toHaveBeenCalledWith(
            expect.objectContaining<Partial<StartScanBody>>({
                username: "alice",
            }),
        );
        expect(history.replaceState).toHaveBeenCalledWith(null, "", "#/scan/scan1");
        expect(mocks.streamScan).toHaveBeenCalledWith(
            "scan1",
            expect.objectContaining({ onDone: expect.any(Function) }),
        );
        expect(store.scan?.status).toBe("finished");
        expect(store.scan?.summary).toEqual(finished.summary);
        expect(store.scan?.identityClusters).toEqual(finished.identity_clusters);
        expect(refreshHistory).toHaveBeenCalled();
    });

    it("loads server scan diff into diff state", async () => {
        mocks.scanDiff.mockResolvedValue({
            from_scan_id: "old",
            to_scan_id: "new",
            added_found: [
                {
                    site: "Mastodon",
                    url: "https://mastodon.example/@alice",
                    kind: "found",
                    elapsed_ms: 10,
                },
            ],
            removed_found: [],
            verdict_changes: [
                { site: "Mastodon", before: "not_found", after: "found" },
            ],
            evidence_changes: [],
        });
        mocks.scan
            .mockResolvedValueOnce({
                status: "finished",
                username: "alice",
                site_count: 1,
                summary: { found: 0, not_found: 1, uncertain: 0 },
                outcomes: [],
                elapsed_ms: 20,
            })
            .mockResolvedValueOnce({
                status: "finished",
                username: "alice",
                site_count: 1,
                summary: { found: 1, not_found: 0, uncertain: 0 },
                outcomes: [
                    {
                        site: "Mastodon",
                        url: "https://mastodon.example/@alice",
                        kind: "found",
                        elapsed_ms: 10,
                    },
                ],
                elapsed_ms: 25,
            });

        await createRoot(async (dispose) => {
            const lifecycle = useScanLifecycle(vi.fn(), vi.fn());
            await lifecycle.startDiff("old", "new", { fromUrl: true });
            dispose();
        });

        expect(mocks.scanDiff).toHaveBeenCalledWith("old", "new");
        expect(mocks.scan).toHaveBeenCalledWith("old");
        expect(mocks.scan).toHaveBeenCalledWith("new");
        expect(store.diff?.scanDiff?.added_found?.[0]?.site).toBe("Mastodon");
        expect(store.diff?.scanDiff?.verdict_changes?.[0]?.after).toBe("found");
    });
});
