import { describe, expect, it } from "vitest";

import { createTestStore } from "./testHelpers";
import { createFilterActions } from "./filterActions";

describe("filterActions", () => {
    it("applies presets while preserving selected egress names", () => {
        const [store, set] = createTestStore({
            filter: { egressNames: ["corp-de"] },
        });
        const actions = createFilterActions({ set, store });

        actions.applyPreset({
            id: "focused",
            label: "Focused",
            filter: {
                tag: ["dev"],
                exclude_tag: ["bot-protected"],
                top: 25,
                nsfw: true,
            },
        });

        expect(store.filter).toEqual({
            presetId: "focused",
            tag: ["dev"],
            excludeTag: ["bot-protected"],
            top: 25,
            nsfw: true,
            egressNames: ["corp-de"],
        });
    });

    it("marks manual edits as custom and toggles tags/egress", () => {
        const [store, set] = createTestStore({
            filter: { tag: ["dev"], egressNames: ["corp-de"] },
        });
        const actions = createFilterActions({ set, store });

        actions.toggleTag("dev");
        actions.toggleTag("social");
        actions.setTop(10);
        actions.setNsfw(true);
        actions.toggleEgress("corp-de");
        actions.toggleEgress("mobile-us");

        expect(store.filter.presetId).toBeNull();
        expect(store.filter.tag).toEqual(["social"]);
        expect(store.filter.top).toBe(10);
        expect(store.filter.nsfw).toBe(true);
        expect(store.filter.egressNames).toEqual(["mobile-us"]);
    });
});
