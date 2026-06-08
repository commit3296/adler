import type { SetStoreFunction } from "solid-js/store";

import type { AppStore } from "../store";

type Toast = { text: string; kind: "success" | "error" | "info" };

interface Deps {
    set: SetStoreFunction<AppStore>;
    store: AppStore;
}

export function createUiActions({ set, store }: Deps) {
    function closeOverlays() {
        set("ui", "drawerOpen", false);
        set("ui", "filtersOpen", false);
        set("ui", "shortcutsOpen", false);
        set("ui", "aboutOpen", false);
        set("ui", "accessOpen", false);
        set("ui", "comparePickerOpen", false);
    }

    return {
        // UI — only one overlay open at a time. Opening any overlay closes
        // the others; the user never sees two stacked dialogs by accident.
        setDrawer(open: boolean) {
            if (open) closeOverlays();
            set("ui", "drawerOpen", open);
        },
        setFilters(open: boolean) {
            if (open) closeOverlays();
            set("ui", "filtersOpen", open);
        },
        setShortcuts(open: boolean) {
            if (open) closeOverlays();
            set("ui", "shortcutsOpen", open);
        },
        setAbout(open: boolean) {
            if (open) closeOverlays();
            set("ui", "aboutOpen", open);
        },
        setAccess(open: boolean) {
            if (open) closeOverlays();
            set("ui", "accessOpen", open);
        },
        setComparePicker(open: boolean) {
            if (open) closeOverlays();
            set("ui", "comparePickerOpen", open);
        },
        toast(text: string, kind: Toast["kind"] = "info") {
            set("ui", "toast", { text, kind });
            setTimeout(() => {
                if (store.ui.toast?.text === text) set("ui", "toast", null);
            }, 2200);
        },
        /// Imperatively dismiss the current toast (e.g. on click).
        setToast(t: Toast | null) {
            set("ui", "toast", t);
        },
    };
}
