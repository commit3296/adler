import type { SetStoreFunction } from "solid-js/store";

import type { AppStore, GroupBy, Sort } from "../store";

interface Deps {
    set: SetStoreFunction<AppStore>;
    store: AppStore;
}

export function createViewActions({ set, store }: Deps) {
    return {
        setSort(s: Sort) {
            set("view", "sort", s);
        },
        setGroupBy(g: GroupBy) {
            set("view", "groupBy", g);
        },
        setResultsFilter(s: string) {
            set("view", "resultsFilter", s);
        },
        toggleShowNotFound() {
            set("view", "showNotFound", !store.view.showNotFound);
        },
        selectSite(s: string | null) {
            set("view", "selectedSite", s);
        },
    };
}
