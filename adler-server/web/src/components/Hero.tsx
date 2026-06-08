import { For, Show, createSignal, type Component } from "solid-js";
import { PRESETS } from "../constants";
import { actions, store } from "../store";

interface Props {
    onSubmit: (username: string) => void;
    onBatch: (usernames: string[]) => void;
}

/// Split a batch textarea into a deduped, trimmed username list.
/// Accepts one-per-line or comma-separated (or a mix).
function parseBatch(raw: string): string[] {
    return [
        ...new Set(
            raw
                .split(/[\n,]/)
                .map((s) => s.trim())
                .filter(Boolean),
        ),
    ];
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

    const [mode, setMode] = createSignal<"single" | "batch">("single");
    const [batchRaw, setBatchRaw] = createSignal("");
    const batchCount = () => parseBatch(batchRaw()).length;

    let inputRef: HTMLInputElement | undefined;

    function submitBatch() {
        const list = parseBatch(batchRaw());
        if (list.length) p.onBatch(list);
    }

    return (
        <section class="hero">
            <div class="hero-inner">
                <h1 class="hero-logo">ADLER</h1>
                <p class="hero-tagline">
                    OSINT username search ·{" "}
                    <Show
                        when={store.catalog.length > 0}
                        fallback={<span class="count-skel" aria-hidden="true" />}
                    >
                        <span class="count">
                            {store.catalog.length.toLocaleString()}
                        </span>
                    </Show>{" "}
                    sites
                </p>

                <div class="hero-mode" role="tablist">
                    <button
                        type="button"
                        class={`hm-tab ${mode() === "single" ? "active" : ""}`}
                        onClick={() => setMode("single")}
                    >
                        Single
                    </button>
                    <button
                        type="button"
                        class={`hm-tab ${mode() === "batch" ? "active" : ""}`}
                        onClick={() => setMode("batch")}
                    >
                        Batch
                    </button>
                </div>

                <Show
                    when={mode() === "single"}
                    fallback={
                        <form
                            class="batch-form"
                            autocomplete="off"
                            onSubmit={(e) => {
                                e.preventDefault();
                                submitBatch();
                            }}
                        >
                            <textarea
                                class="batch-input"
                                rows={5}
                                placeholder={"one username per line\n\nlinus\ntorvalds\noctocat"}
                                autofocus
                                value={batchRaw()}
                                onInput={(e) => setBatchRaw(e.currentTarget.value)}
                                onKeyDown={(e) => {
                                    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
                                        e.preventDefault();
                                        submitBatch();
                                    }
                                }}
                            />
                            <div class="batch-actions">
                                <span class="batch-hint">
                                    {batchCount()} username
                                    {batchCount() === 1 ? "" : "s"} ·{" "}
                                    <kbd>⌘</kbd>
                                    <kbd>↵</kbd> to run
                                </span>
                                <button
                                    class="scan-btn"
                                    type="submit"
                                    disabled={batchCount() === 0}
                                >
                                    Scan {batchCount() || ""}
                                </button>
                            </div>
                        </form>
                    }
                >
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
                </Show>

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
                                <Show when={store.catalog.length > 0}>
                                    <span class="count">
                                        {countPreset(p.id).toLocaleString()}
                                    </span>
                                </Show>
                            </button>
                        )}
                    </For>
                </div>
                <button class="advanced-link" onClick={() => actions.setFilters(true)}>
                    Advanced filters
                </button>
                <Show when={store.disabledCatalog.length > 0}>
                    <p class="parked-note">
                        {store.disabledCatalog.length.toLocaleString()} parked{" "}
                        {store.disabledCatalog.length === 1 ? "site" : "sites"} excluded ·{" "}
                        {store.disabledCatalog[0]?.name}:{" "}
                        {store.disabledCatalog[0]?.disabled_reason}
                    </p>
                </Show>
            </div>
        </section>
    );
};
