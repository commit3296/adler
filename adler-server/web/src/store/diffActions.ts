import type { SetStoreFunction } from "solid-js/store";

import type { AppStore, DiffState } from "../store";

interface Deps {
    set: SetStoreFunction<AppStore>;
}

export function createDiffActions({ set }: Deps) {
    return {
        setDiff(d: DiffState | null) {
            set("diff", d);
            if (d) {
                set("scan", null);
                set("notFound", null);
                set("loading", false);
            }
        },
        armCompare(id: string | null) {
            set("ui", "compareArmed", id);
        },
    };
}
