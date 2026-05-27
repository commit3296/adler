import { Show, createMemo, type Component } from "solid-js";
import { PRESETS } from "../constants";
import { actions, store } from "../store";
import { fmtElapsed } from "../lib/format";
import { Button } from "../ui";

interface Props {
    onRescan: () => void;
    onStop: () => void;
    onContinue: () => void;
    onRestart: () => void;
    onExitDiff: () => void;
    onCompareWithPrevious: () => void;
}

export const ScanHeader: Component<Props> = (p) => {
    /// Whether the current scan's username has at least one *other*
    /// finished scan we could diff against. Drives the visibility of
    /// the "Compare with previous" affordance.
    const hasPrevious = createMemo<boolean>(() => {
        const cur = store.scan;
        if (!cur || cur.status !== "finished") return false;
        return store.history.some(
            (h) =>
                h.username === cur.username &&
                h.scan_id !== cur.id &&
                h.status === "finished",
        );
    });

    function statePillClass(): string {
        if (store.diff) return "state-pill done";
        if (!store.scan) return "state-pill idle";
        if (store.scan.status === "running") return "state-pill scanning";
        if (store.scan.status === "stopped") return "state-pill stopped";
        return "state-pill done";
    }

    function stateText(): string {
        if (store.diff) return "Diff";
        if (!store.scan) return "Idle";
        if (store.scan.status === "running") return "Scanning";
        if (store.scan.status === "stopped") return "Stopped";
        return "Done";
    }

    return (
        <div class="scan-header">
            <div class="left">
                {/* Diff mode: back-arrow lives on the LEFT (back/prev
                    convention), title shows the comparison. */}
                <Show when={store.diff}>
                    <div class="diff-titlebar">
                        <Button variant="ghost" size="sm" onClick={p.onExitDiff}>
                            ← Back
                        </Button>
                        <div class="diff-title">
                            <span class="diff-title-label">Diff:</span>
                            <span class="diff-title-users">
                                <span class="diff-prev">@{store.diff!.a.username}</span>
                                <span class="diff-arrow">→</span>
                                <span class="diff-cur">@{store.diff!.b.username}</span>
                            </span>
                        </div>
                    </div>
                </Show>
                <Show when={!store.diff}>
                    <div class="scan-username">
                        <span class="at">@</span>
                        <span>{store.scan?.username ?? ""}</span>
                    </div>
                    <div class="scan-scope">
                        <Show when={store.scan}>
                            <span
                                class="scope-chip clickable"
                                title="Open filters"
                                onClick={() => actions.setFilters(true)}
                            >
                                {store.filter.presetId
                                    ? PRESETS.find((p) => p.id === store.filter.presetId)
                                          ?.label ?? "Custom"
                                    : "Custom"}
                                <span
                                    style={{
                                        color: "var(--color-fg-faint)",
                                        "font-size": "0.7rem",
                                    }}
                                >
                                    {store.scan!.siteCount.toLocaleString()}
                                </span>
                            </span>
                            <span
                                style={{
                                    color: "var(--color-fg-faint)",
                                    "font-size": "0.72rem",
                                }}
                            >
                                of {store.catalog.length.toLocaleString()}
                            </span>
                        </Show>
                    </div>
                </Show>
            </div>
            <div class="right">
                <div class="status-row">
                    <span class={statePillClass()}>{stateText()}</span>
                    <Show when={store.scan && store.scan.summary && store.scan.summary.found > 0}>
                        <span class="dot-counter found">
                            <strong>{store.scan!.summary!.found}</strong> found
                        </span>
                    </Show>
                    <Show
                        when={
                            store.scan && store.scan.summary && store.scan.summary.uncertain > 0
                        }
                    >
                        <span class="dot-counter uncertain">
                            <strong>{store.scan!.summary!.uncertain}</strong> uncertain
                        </span>
                    </Show>
                    <Show when={store.scan && store.scan.status === "running"}>
                        <span class="dot-counter">
                            <strong>{store.scan!.outcomes.length}</strong>/
                            {store.scan!.siteCount}
                        </span>
                    </Show>
                    <Show when={store.scan}>
                        <span class="elapsed-cell">{fmtElapsed(store.scan!.elapsedMs)}</span>
                    </Show>
                </div>
                <Show when={store.scan}>
                    <div class="scan-actions">
                        <Show when={store.scan!.status === "running"}>
                            <Button variant="ghost" size="sm" danger onClick={p.onStop}>
                                Stop
                            </Button>
                        </Show>
                        <Show when={store.scan!.status === "stopped"}>
                            <Button variant="secondary" size="sm" onClick={p.onContinue}>
                                Continue
                            </Button>
                            <Button variant="ghost" size="sm" onClick={p.onRestart}>
                                Restart
                            </Button>
                        </Show>
                        <Show when={store.scan!.status === "finished"}>
                            <Show when={hasPrevious()}>
                                <Button
                                    variant="ghost"
                                    size="sm"
                                    onClick={p.onCompareWithPrevious}
                                >
                                    Compare with previous
                                </Button>
                            </Show>
                            <Button variant="ghost" size="sm" onClick={p.onRescan}>
                                Rescan
                            </Button>
                        </Show>
                    </div>
                </Show>
                {/* Exit moved to the LEFT of the header (see above) — the
                    right column shows only diff stats. */}
            </div>
        </div>
    );
};
