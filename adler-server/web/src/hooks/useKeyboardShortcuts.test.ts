import { describe, expect, it, vi } from "vitest";

import type { CheckOutcome } from "../types";
import { createTestStore } from "../store/testHelpers";
import { createKeyboardHandler } from "./useKeyboardShortcuts";

function keyEvent(key: string, target: { tagName?: string } = {}): KeyboardEvent {
    return {
        key,
        target,
        preventDefault: vi.fn(),
    } as unknown as KeyboardEvent;
}

function row(site: string) {
    return {
        dataset: { site },
        scrollIntoView: vi.fn(),
        style: { outline: "", outlineOffset: "" },
    } as unknown as HTMLElement;
}

const github: CheckOutcome = {
    site: "GitHub",
    url: "https://github.com/alice",
    kind: "found",
    elapsed_ms: 12,
};

describe("createKeyboardHandler", () => {
    it("applies the Escape cascade before stopping a scan", () => {
        const [store, set] = createTestStore({
            ui: { shortcutsOpen: true },
        });
        const actions = {
            selectSite: vi.fn(),
            setAbout: vi.fn(),
            setDrawer: vi.fn(),
            setFilters: vi.fn(),
            setShortcuts: vi.fn((open: boolean) =>
                set("ui", "shortcutsOpen", open),
            ),
            toast: vi.fn(),
            toggleShowNotFound: vi.fn(),
        };
        const lifecycle = {
            exitDiff: vi.fn(),
            isStreaming: vi.fn(() => true),
            stopScan: vi.fn(),
        };
        const handler = createKeyboardHandler({
            actions,
            clipboard: { writeText: vi.fn() },
            getInput: () => null,
            getRows: () => [],
            lifecycle,
            openWindow: vi.fn(),
            store,
        });

        handler(keyEvent("Escape"));

        expect(actions.setShortcuts).toHaveBeenCalledWith(false);
        expect(lifecycle.stopScan).not.toHaveBeenCalled();
    });

    it("moves result selection with keyboard navigation", () => {
        const [store, set] = createTestStore();
        const rows = [row("GitHub"), row("GitLab")];
        const actions = {
            selectSite: vi.fn((site: string | null) => {
                set("view", "selectedSite", site);
            }),
            setAbout: vi.fn(),
            setDrawer: vi.fn(),
            setFilters: vi.fn(),
            setShortcuts: vi.fn(),
            toast: vi.fn(),
            toggleShowNotFound: vi.fn(),
        };
        const handler = createKeyboardHandler({
            actions,
            clipboard: { writeText: vi.fn() },
            getInput: () => null,
            getRows: () => rows,
            lifecycle: {
                exitDiff: vi.fn(),
                isStreaming: vi.fn(() => false),
                stopScan: vi.fn(),
            },
            openWindow: vi.fn(),
            store,
        });

        handler(keyEvent("j"));
        handler(keyEvent("j"));

        expect(actions.selectSite).toHaveBeenLastCalledWith("GitLab");
        expect(rows[1]!.style.outline).toBe("2px solid var(--red)");
    });

    it("opens and copies the selected outcome URL", async () => {
        const [store] = createTestStore({
            scan: {
                id: "scan-1",
                username: "alice",
                filterAtStart: {
                    tag: [],
                    excludeTag: [],
                    top: null,
                    nsfw: false,
                    egressNames: [],
                },
                outcomes: [github],
                outcomeSites: { GitHub: true },
                bucketsByCategory: { dev: [github] },
                status: "finished",
                summary: { found: 1, not_found: 0, uncertain: 0 },
                siteCount: 1,
                startedAtMs: 0,
                elapsedMs: 12,
            },
            view: { selectedSite: "GitHub" },
        });
        const clipboard = { writeText: vi.fn(() => Promise.resolve()) };
        const openWindow = vi.fn();
        const toast = vi.fn();
        const handler = createKeyboardHandler({
            actions: {
                selectSite: vi.fn(),
                setAbout: vi.fn(),
                setDrawer: vi.fn(),
                setFilters: vi.fn(),
                setShortcuts: vi.fn(),
                toast,
                toggleShowNotFound: vi.fn(),
            },
            clipboard,
            getInput: () => null,
            getRows: () => [],
            lifecycle: {
                exitDiff: vi.fn(),
                isStreaming: vi.fn(() => false),
                stopScan: vi.fn(),
            },
            openWindow,
            store,
        });

        handler(keyEvent("o"));
        handler(keyEvent("c"));
        await Promise.resolve();

        expect(openWindow).toHaveBeenCalledWith(
            "https://github.com/alice",
            "_blank",
            "noopener",
        );
        expect(clipboard.writeText).toHaveBeenCalledWith("https://github.com/alice");
        expect(toast).toHaveBeenCalledWith("URL copied", "success");
    });
});
