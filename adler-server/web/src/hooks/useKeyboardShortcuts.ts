import { onCleanup, onMount } from "solid-js";

import { displayUrl } from "../lib/format";
import { actions, store } from "../store";
import type { CheckOutcome } from "../types";
import type { useScanLifecycle } from "./useScanLifecycle";

type ScanLifecycle = ReturnType<typeof useScanLifecycle>;

interface KeyboardDeps {
    actions: Pick<
        typeof actions,
        | "selectSite"
        | "setAbout"
        | "setDrawer"
        | "setFilters"
        | "setShortcuts"
        | "toast"
        | "toggleShowNotFound"
    >;
    clipboard: Pick<Clipboard, "writeText">;
    getInput: () => HTMLInputElement | null;
    getRows: () => HTMLElement[];
    lifecycle: Pick<ScanLifecycle, "exitDiff" | "isStreaming" | "stopScan">;
    openWindow: (url: string, target: string, features: string) => void;
    store: typeof store;
}

export function createKeyboardHandler({
    actions,
    clipboard,
    getInput,
    getRows,
    lifecycle,
    openWindow,
    store,
}: KeyboardDeps): (e: KeyboardEvent) => void {
    function moveSelection(delta: number) {
        const rows = getRows();
        if (rows.length === 0) return;
        const sites = rows.map((r) => r.dataset.site!);
        let idx = store.view.selectedSite ? sites.indexOf(store.view.selectedSite) : -1;
        idx = Math.max(0, Math.min(rows.length - 1, idx + delta));
        for (const r of rows) r.style.outline = "";
        rows[idx]!.style.outline = "2px solid var(--red)";
        rows[idx]!.style.outlineOffset = "-1px";
        rows[idx]!.scrollIntoView({ block: "nearest" });
        actions.selectSite(sites[idx]!);
    }

    function selectedOutcome(): CheckOutcome | null {
        if (!store.scan || !store.view.selectedSite) return null;
        return store.scan.outcomes.find((o) => o.site === store.view.selectedSite) ?? null;
    }

    function handleKey(e: KeyboardEvent) {
        const tag = (e.target as HTMLElement | null)?.tagName?.toLowerCase() ?? "";
        const inField = tag === "input" || tag === "textarea";

        if (e.key === "Escape") {
            if (store.ui.shortcutsOpen) return actions.setShortcuts(false);
            if (store.ui.aboutOpen) return actions.setAbout(false);
            if (store.ui.filtersOpen) return actions.setFilters(false);
            if (store.ui.drawerOpen) return actions.setDrawer(false);
            if (lifecycle.isStreaming()) return lifecycle.stopScan();
            if (store.diff) return lifecycle.exitDiff();
            return;
        }
        if (inField) return;

        if (e.key === "/") {
            e.preventDefault();
            const el = getInput();
            if (el) {
                el.focus();
                el.select();
            }
            return;
        }
        if (e.key === "?") {
            e.preventDefault();
            actions.setShortcuts(true);
            return;
        }
        if (e.key === "h") {
            e.preventDefault();
            actions.setDrawer(!store.ui.drawerOpen);
            return;
        }
        if (e.key === "f") {
            e.preventDefault();
            actions.setFilters(true);
            return;
        }
        if (e.key === "n") return actions.toggleShowNotFound();
        if (e.key === "j" || e.key === "ArrowDown") {
            e.preventDefault();
            moveSelection(1);
            return;
        }
        if (e.key === "k" || e.key === "ArrowUp") {
            e.preventDefault();
            moveSelection(-1);
            return;
        }
        if (e.key === "o") {
            const o = selectedOutcome();
            if (o) openWindow(displayUrl(o.url), "_blank", "noopener");
            return;
        }
        if (e.key === "c") {
            const o = selectedOutcome();
            if (!o) return;
            clipboard
                .writeText(displayUrl(o.url))
                .then(() => actions.toast("URL copied", "success"))
                .catch(() => actions.toast("Copy blocked", "error"));
        }
    }

    return handleKey;
}

export function useKeyboardShortcuts(lifecycle: ScanLifecycle): void {
    const handleKey = createKeyboardHandler({
        actions,
        clipboard: navigator.clipboard,
        getInput: () => document.getElementById("username") as HTMLInputElement | null,
        getRows: () => Array.from(document.querySelectorAll<HTMLElement>(".result-row")),
        lifecycle,
        openWindow: (url, target, features) => window.open(url, target, features),
        store,
    });

    onMount(() => window.addEventListener("keydown", handleKey));
    onCleanup(() => window.removeEventListener("keydown", handleKey));
}
