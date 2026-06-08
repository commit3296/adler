import { produce, type SetStoreFunction } from "solid-js/store";

import type { Preset } from "../constants";
import type { AppStore, FilterState } from "../store";

interface Deps {
    set: SetStoreFunction<AppStore>;
    store: AppStore;
}

export function createFilterActions({ set, store }: Deps) {
    return {
        applyPreset(p: Preset) {
            const filter: FilterState = {
                presetId: p.id,
                tag: (p.filter.tag ?? []).slice(),
                excludeTag: (p.filter.exclude_tag ?? []).slice(),
                top: p.filter.top ?? null,
                nsfw: !!p.filter.nsfw,
                // Egress is orthogonal to the catalog presets — preserve
                // it across preset switches so the operator's transport
                // choice doesn't silently flip.
                egressNames: store.filter.egressNames.slice(),
            };
            set("filter", filter);
        },
        toggleTag(t: string) {
            set("filter", "presetId", null);
            set(
                "filter",
                "tag",
                produce((tags: string[]) => {
                    const idx = tags.indexOf(t);
                    if (idx >= 0) tags.splice(idx, 1);
                    else tags.push(t);
                }),
            );
        },
        removeTag(t: string) {
            set("filter", "presetId", null);
            set("filter", "tag", (tags) => tags.filter((x) => x !== t));
        },
        removeExcludeTag(t: string) {
            set("filter", "presetId", null);
            set("filter", "excludeTag", (tags) => tags.filter((x) => x !== t));
        },
        setTop(n: number | null) {
            set("filter", "presetId", null);
            set("filter", "top", n);
        },
        setNsfw(on: boolean) {
            set("filter", "presetId", null);
            set("filter", "nsfw", on);
        },
        toggleEgress(name: string) {
            set(
                "filter",
                "egressNames",
                produce((names: string[]) => {
                    const idx = names.indexOf(name);
                    if (idx >= 0) names.splice(idx, 1);
                    else names.push(name);
                }),
            );
        },
        clearEgress() {
            set("filter", "egressNames", []);
        },
    };
}
