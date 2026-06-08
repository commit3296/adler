import type { SetStoreFunction } from "solid-js/store";

import { categoryForTags } from "../constants";
import type { AccessResponse, ScanListEntry, SitesResponse } from "../types";
import type { AppStore } from "../store";

interface Deps {
    set: SetStoreFunction<AppStore>;
    store: AppStore;
}

export function createCatalogActions({ set, store }: Deps) {
    return {
        setCatalog(response: SitesResponse) {
            const sites = response.sites;
            const tagsBySite: Record<string, string[]> = {};
            const categoryBySite: Record<string, string> = {};
            const tagCounts: Record<string, number> = {};
            for (const s of sites) {
                tagsBySite[s.name] = s.tags;
                categoryBySite[s.name] = categoryForTags(s.tags).id;
                for (const t of s.tags) tagCounts[t] = (tagCounts[t] ?? 0) + 1;
            }
            set("catalog", sites);
            set("disabledCatalog", response.disabled);
            set("tagsBySite", tagsBySite);
            set("categoryBySite", categoryBySite);
            set("tagCounts", tagCounts);
        },
        setHistory(list: ScanListEntry[]) {
            set("history", list);
        },
        setAccessConfig(a: AccessResponse) {
            set("accessConfig", a);
            // Prune any selected egress that's no longer in the pool — a
            // post-restart subset rotation should clear stale chip state.
            const known = new Set(
                a.egress.map((e) => e.name).filter((n): n is string => !!n),
            );
            const stale = store.filter.egressNames.filter((n) => !known.has(n));
            if (stale.length > 0) {
                set(
                    "filter",
                    "egressNames",
                    store.filter.egressNames.filter((n) => known.has(n)),
                );
            }
        },
    };
}
