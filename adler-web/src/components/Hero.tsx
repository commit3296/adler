import { For, type Component } from "solid-js";
import { PRESETS } from "../constants";
import { actions, store } from "../store";

interface Props {
    onSubmit: (username: string) => void;
}

export const Hero: Component<Props> = (p) => {
    /// Number of sites a given preset would actually scan against
    /// the loaded catalog. Live-updates with the catalog.
    function countPreset(presetId: string): number {
        const preset = PRESETS.find((x) => x.id === presetId);
        if (!preset) return 0;
        const f = preset.filter;
        const tag = f.tag ?? [];
        const excludeTag = f.exclude_tag ?? [];
        const top = f.top ?? null;
        const nsfw = !!f.nsfw;
        return store.catalog.filter((s) => {
            const tags = s.tags;
            if (!nsfw && tags.includes("nsfw")) return false;
            if (tag.length && !tag.some((t) => tags.includes(t))) return false;
            if (excludeTag.some((t) => tags.includes(t))) return false;
            if (top != null && (s.popularity == null || s.popularity > top))
                return false;
            return true;
        }).length;
    }

    let inputRef: HTMLInputElement | undefined;

    return (
        <section class="hero">
            <div class="hero-inner">
                <h1 class="hero-logo">ADLER</h1>
                <p class="hero-tagline">
                    OSINT username search ·{" "}
                    <span class="count">{store.catalog.length.toLocaleString()}</span>{" "}
                    sites
                </p>
                <form
                    class="search-form"
                    autocomplete="off"
                    onSubmit={(e) => {
                        e.preventDefault();
                        const u = inputRef?.value.trim();
                        if (u) p.onSubmit(u);
                    }}
                >
                    <input
                        id="username"
                        ref={inputRef}
                        type="text"
                        placeholder="username"
                        required
                        minlength="1"
                        autofocus
                    />
                    <button class="scan-btn" type="submit">
                        Scan
                    </button>
                </form>
                <div class="preset-row">
                    <For each={PRESETS}>
                        {(p) => (
                            <button
                                type="button"
                                class={`preset ${
                                    store.filter.presetId === p.id ? "active" : ""
                                }`}
                                onClick={() => actions.applyPreset(p)}
                            >
                                {p.label}
                                <span class="count">
                                    {countPreset(p.id).toLocaleString()}
                                </span>
                            </button>
                        )}
                    </For>
                </div>
                <button class="advanced-link" onClick={() => actions.setFilters(true)}>
                    Advanced filters
                </button>
            </div>
        </section>
    );
};
