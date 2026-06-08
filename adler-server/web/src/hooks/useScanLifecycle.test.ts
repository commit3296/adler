import { createRoot } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { FinishedScan, StartScanBody } from "../types";

const mocks = vi.hoisted(() => ({
    startScan: vi.fn(),
    streamScan: vi.fn(),
}));

vi.mock("../api", async () => {
    const actual = await vi.importActual<typeof import("../api")>("../api");
    return {
        ...actual,
        api: {
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
        expect(refreshHistory).toHaveBeenCalled();
    });
});
