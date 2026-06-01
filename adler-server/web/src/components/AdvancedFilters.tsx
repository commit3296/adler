import { For, Show, createMemo, createSignal, type Component } from "solid-js";
import { PRESETS } from "../constants";
import { actions, store } from "../store";
import { Button, Chip, Input, Modal } from "../ui";

export const AdvancedFilters: Component = () => {
    const [tagSearch, setTagSearch] = createSignal("");
    /// Mid-scan filter edits would never reach the running probe —
    /// the backend was handed its site list at start. Disable all
    /// inputs here while a scan is live and show a one-line banner
    /// explaining the constraint. (Sorting / grouping / hide-not-found
    /// in the results toolbar are not blocked — those are pure
    /// view-state.)
    const isScanning = createMemo(() => store.scan?.status === "running");

    const tags = createMemo(() => {
        const q = tagSearch().trim().toLowerCase();
        return Object.entries(store.tagCounts)
            .filter(([t]) => !q || t.toLowerCase().includes(q))
            .sort((a, b) => b[1] - a[1])
            .slice(0, 200);
    });

    const activeChips = createMemo(() => {
        if (store.filter.presetId && store.filter.presetId !== "all") {
            const p = PRESETS.find((x) => x.id === store.filter.presetId);
            if (p) return [{ kind: "preset" as const, label: p.label }];
        }
        const out: {
            kind: "tag" | "extag" | "top" | "nsfw" | "egress";
            label: string;
        }[] = [];
        for (const t of store.filter.tag) out.push({ kind: "tag", label: t });
        for (const t of store.filter.excludeTag) out.push({ kind: "extag", label: t });
        if (store.filter.top != null)
            out.push({ kind: "top", label: `top ≤ ${store.filter.top}` });
        if (store.filter.nsfw) out.push({ kind: "nsfw", label: "nsfw" });
        for (const e of store.filter.egressNames)
            out.push({ kind: "egress", label: `egress: ${e}` });
        return out;
    });

    /// Named egresses available for per-scan subset selection.
    /// Filter out unnamed ones up front — those can't be referenced by
    /// name and shouldn't appear as toggleable controls.
    const egresses = createMemo(() =>
        (store.accessConfig?.egress ?? []).filter(
            (e): e is { name: string; country?: string; kind: typeof e.kind } =>
                !!e.name,
        ),
    );

    function clearChip(c: { kind: string; label: string }) {
        if (c.kind === "preset") actions.applyPreset(PRESETS.find((p) => p.id === "all")!);
        else if (c.kind === "tag") actions.removeTag(c.label);
        else if (c.kind === "extag") actions.removeExcludeTag(c.label);
        else if (c.kind === "top") actions.setTop(null);
        else if (c.kind === "nsfw") actions.setNsfw(false);
        else if (c.kind === "egress")
            actions.toggleEgress(c.label.replace(/^egress: /, ""));
    }

    return (
        <Modal
            open={store.ui.filtersOpen}
            onClose={() => actions.setFilters(false)}
            title="Advanced filters"
            footer={
                <>
                    <Button
                        variant="ghost"
                        size="sm"
                        disabled={isScanning()}
                        onClick={() =>
                            actions.applyPreset(PRESETS.find((p) => p.id === "quick")!)
                        }
                    >
                        Reset
                    </Button>
                    <Button
                        variant="primary"
                        size="sm"
                        onClick={() => actions.setFilters(false)}
                    >
                        Done
                    </Button>
                </>
            }
        >
            <Show when={isScanning()}>
                <div class="filters-locked-banner">
                    <span>Scan in progress.</span>
                    <span class="dim">Filters apply to your next scan — stop or wait.</span>
                </div>
            </Show>
            <fieldset
                class="filters-fieldset"
                disabled={isScanning()}
                aria-busy={isScanning()}
            >
                <Show when={activeChips().length > 0}>
                    <div class="active-filters">
                        <span class="summary-label">Active:</span>
                        <For each={activeChips()}>
                            {(c) => (
                                <Chip
                                    variant={c.kind === "extag" ? "exclude" : "include"}
                                    disabled={isScanning()}
                                    onDismiss={() => clearChip(c)}
                                >
                                    {c.label}
                                </Chip>
                            )}
                        </For>
                    </div>
                </Show>

                <label class="form-label">Tags</label>
                <Input
                    placeholder="Search tags — e.g. forum, region:ru"
                    value={tagSearch()}
                    onInput={(e) => setTagSearch(e.currentTarget.value)}
                />
                <div class="tag-grid">
                    <For each={tags()}>
                        {([t, n]) => {
                            const active = () => store.filter.tag.includes(t);
                            return (
                                <label class={`tag-check ${active() ? "active" : ""}`}>
                                    <input
                                        type="checkbox"
                                        checked={active()}
                                        onChange={() => actions.toggleTag(t)}
                                    />
                                    {t}
                                    <span class="ct">{n}</span>
                                </label>
                            );
                        }}
                    </For>
                </div>

                <label class="form-label" for="top-n">
                    Top N most popular
                </label>
                <Input
                    id="top-n"
                    type="number"
                    min="1"
                    placeholder="Empty to disable · only ~35 sites have a rank"
                    value={store.filter.top ?? ""}
                    onInput={(e) => {
                        const v = parseInt(e.currentTarget.value, 10);
                        actions.setTop(Number.isFinite(v) && v > 0 ? v : null);
                    }}
                />

                <label class="form-label">Adult content</label>
                <label class="checkbox-row">
                    <input
                        type="checkbox"
                        checked={store.filter.nsfw}
                        onChange={(e) => actions.setNsfw(e.currentTarget.checked)}
                    />
                    Include sites tagged <code>nsfw</code>
                </label>

                <Show when={egresses().length > 0}>
                    <label class="form-label">
                        Egress (per-scan subset)
                    </label>
                    <p class="form-help">
                        Restrict this scan to a subset of the loaded{" "}
                        <code>--proxy-pool</code>. Empty = full pool. Sites
                        whose access policy can't be satisfied by your
                        subset land in{" "}
                        <code>Uncertain(geo_unavailable)</code> — same
                        honest verdict as if no egress matched at all.
                    </p>
                    <div class="tag-grid">
                        <For each={egresses()}>
                            {(e) => {
                                const active = () =>
                                    store.filter.egressNames.includes(e.name);
                                return (
                                    <label
                                        class={`tag-check ${active() ? "active" : ""}`}
                                    >
                                        <input
                                            type="checkbox"
                                            checked={active()}
                                            onChange={() =>
                                                actions.toggleEgress(e.name)
                                            }
                                        />
                                        {e.name}
                                        <span class="ct">
                                            {e.country ?? "—"}/{e.kind}
                                        </span>
                                    </label>
                                );
                            }}
                        </For>
                    </div>
                </Show>
            </fieldset>
        </Modal>
    );
};
