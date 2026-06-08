import { describe, expect, it, vi } from "vitest";

import { createHashRouteHandler } from "./useHashRouting";

function lifecycle() {
    return {
        closeStream: vi.fn(),
        loadScan: vi.fn(),
        startDiff: vi.fn(),
        stopElapsedTimer: vi.fn(),
    };
}

function actions() {
    return {
        clearBatch: vi.fn(),
        clearScan: vi.fn(),
        setNotFound: vi.fn(),
    };
}

describe("createHashRouteHandler", () => {
    it("loads scan routes", () => {
        const lc = lifecycle();
        const handler = createHashRouteHandler({
            actions: actions(),
            getHash: () => "#/scan/scan1",
            lifecycle: lc,
            setRouteHash: vi.fn(),
            store: { diff: null },
        });

        handler();

        expect(lc.loadScan).toHaveBeenCalledWith("scan1");
        expect(lc.closeStream).not.toHaveBeenCalled();
    });

    it("starts diff routes unless already showing that diff", () => {
        const lc = lifecycle();
        const setRouteHash = vi.fn();
        const handler = createHashRouteHandler({
            actions: actions(),
            getHash: () => "#/diff/a/b",
            lifecycle: lc,
            setRouteHash,
            store: { diff: null },
        });

        handler();

        expect(setRouteHash).toHaveBeenCalledWith("#/diff/a/b");
        expect(lc.startDiff).toHaveBeenCalledWith("a", "b", { fromUrl: true });
    });

    it("clears active state on home routes", () => {
        const act = actions();
        const lc = lifecycle();
        const handler = createHashRouteHandler({
            actions: act,
            getHash: () => "#/",
            lifecycle: lc,
            setRouteHash: vi.fn(),
            store: { diff: null },
        });

        handler();

        expect(lc.closeStream).toHaveBeenCalled();
        expect(lc.stopElapsedTimer).toHaveBeenCalled();
        expect(act.clearBatch).toHaveBeenCalled();
        expect(act.clearScan).toHaveBeenCalled();
    });

    it("marks unknown routes as not found", () => {
        const act = actions();
        const handler = createHashRouteHandler({
            actions: act,
            getHash: () => "#/wat",
            lifecycle: lifecycle(),
            setRouteHash: vi.fn(),
            store: { diff: null },
        });

        handler();

        expect(act.setNotFound).toHaveBeenCalledWith({
            kind: "route",
            detail: "#/wat",
        });
    });
});
