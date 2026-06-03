import { For, Show, createMemo, createSignal, type Component } from "solid-js";
import { PRESETS } from "../constants";
import { actions, store } from "../store";
import { Button, Chip, Input, Modal } from "../ui";

interface Props {
    /// Cancel the running scan and replace it with a successor driven
    /// by the current filter (via `POST /api/scan/:id/refilter`). Wired
    /// by App.tsx; the button stays hidden when no scan is running or
    /// the live filter matches the running scan's snapshot.
    onRefilter: () => void;
}

/// Order-insensitive equality on string arrays. Filters store tags as
/// `string[]` but the operator's order doesn't drive scan behaviour —
/// compare by set membership.
function sameStringSet(a: string[], b: string[]): boolean {
    if (a.length !== b.length) return false;
    const sa = new Set(a);
    for (const v of b) if (!sa.has(v)) return false;
    return true;
}

export const AdvancedFilters: Component<Props> = (p) => {
    const [tagSearch, setTagSearch] = createSignal("");
    const isScanning = createMemo(() => store.scan?.status === "running");
    /// Effective filter differs from the snapshot the running scan was
    /// launched with — surfaces the refilter call-to-action. Compared
    /// field-by-field rather than via JSON because the underlying
    /// arrays are stable references the operator may have only
    /// shuffled.
    const isDivergent = createMemo<boolean>(() => {
        const s = store.scan;
        if (!s || s.status !== "running") return false;
        const a = s.filterAtStart;
        const b = store.filter;
        if (a.top !== b.top || a.nsfw !== b.nsfw) return true;
        if (!sameStringSet(a.tag, b.tag)) return true;
        if (!sameStringSet(a.excludeTag, b.excludeTag)) return true;
        if (!sameStringSet(a.egressNames, b.egressNames)) return true;
        return false;
    });

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
                        onClick={() =>
                            actions.applyPreset(PRESETS.find((p) => p.id === "quick")!)
                        }
                    >
                        Reset
                    </Button>
                    <Show when={isScanning() && isDivergent()}>
                        <Button
                            variant="primary"
                            size="sm"
                            onClick={() => {
                                p.onRefilter();
                                actions.setFilters(false);
                            }}
                        >
                            Apply (re-scan)
                        </Button>
                    </Show>
                    <Show when={!(isScanning() && isDivergent())}>
                        <Button
                            variant="primary"
                            size="sm"
                            onClick={() => actions.setFilters(false)}
                        >
                            Done
                        </Button>
                    </Show>
                </>
            }
        >
            <Show when={isScanning() && isDivergent()}>
                <div class="filters-locked-banner">
                    <span>Scan in progress — filter differs from what it was launched with.</span>
                    <span class="dim">
                        Apply (re-scan) cancels the live scan and starts a successor;
                        sites already done carry over without re-probing.
                    </span>
                </div>
            </Show>
            <fieldset class="filters-fieldset">
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
