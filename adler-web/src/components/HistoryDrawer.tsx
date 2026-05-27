import { For, Show, createMemo, createSignal, type Component } from "solid-js";
import { actions, store } from "../store";
import { fmtAgo } from "../lib/format";
import type { ScanListEntry } from "../types";
import { Modal, SearchInput } from "../ui";

interface Props {
    onOpenScan: (id: string) => void;
    onStartDiff: (aId: string, bId: string) => void;
}

/// History modal: list of past scans with an in-modal search.
/// The component name kept (`HistoryDrawer`) for back-compat with App.tsx.
export const HistoryDrawer: Component<Props> = (p) => {
    const [query, setQuery] = createSignal("");

    const filtered = createMemo<ScanListEntry[]>(() => {
        const q = query().trim().toLowerCase();
        if (!q) return store.history;
        return store.history.filter(
            (h) =>
                h.username.toLowerCase().includes(q) || h.scan_id.toLowerCase().includes(q),
        );
    });

    function summaryText(h: ScanListEntry) {
        if (!h.summary) return <span class="a">Scanning…</span>;
        const total = h.summary.found + h.summary.uncertain + h.summary.not_found;
        return (
            <>
                <span class="g">{h.summary.found} found</span>
                <Show when={h.summary.uncertain > 0}>
                    <span class="sep">·</span>
                    <span class="a">{h.summary.uncertain} uncertain</span>
                </Show>
                <span class="sep">·</span>
                <span class="nf">of {total.toLocaleString()} sites</span>
            </>
        );
    }

    function handleCompare(id: string) {
        if (store.ui.compareArmed === id) {
            actions.armCompare(null);
            actions.toast("Compare cancelled", "info");
        } else if (store.ui.compareArmed) {
            p.onStartDiff(store.ui.compareArmed, id);
            actions.armCompare(null);
            actions.setDrawer(false);
        } else {
            actions.armCompare(id);
            actions.toast("Pick another scan to compare", "info");
        }
    }
    function compareLabel(id: string): string {
        if (store.ui.compareArmed === id) return "Cancel";
        if (store.ui.compareArmed) return "Pick";
        return "Compare";
    }

    return (
        <Modal
            open={store.ui.drawerOpen}
            onClose={() => actions.setDrawer(false)}
            maxWidth="34rem"
            title={
                <>
                    History <span class="modal-count">{store.history.length}</span>
                </>
            }
            headerSlot={
                <div class="modal-search-area">
                    <SearchInput
                        placeholder="Search by username or scan id"
                        value={query()}
                        onInput={setQuery}
                        autofocus
                    />
                </div>
            }
        >
            <Show
                when={filtered().length > 0}
                fallback={
                    <div class="history-empty">
                        <Show
                            when={query().length > 0}
                            fallback={
                                <>
                                    No scans yet
                                    <span class="hint">Run a scan to see it here</span>
                                </>
                            }
                        >
                            Nothing matches "{query()}"
                        </Show>
                    </div>
                }
            >
                <For each={filtered()}>
                    {(h) => (
                        <div
                            class={`history-row ${
                                store.scan?.id === h.scan_id ? "active" : ""
                            } ${store.ui.compareArmed === h.scan_id ? "armed" : ""}`}
                            onClick={() => p.onOpenScan(h.scan_id)}
                        >
                            <div class="h-top">
                                <span class="h-user">@{h.username}</span>
                                <span class="h-time">{fmtAgo(h.started_at_ms)}</span>
                            </div>
                            <div class="h-summary">{summaryText(h)}</div>
                            <div class="h-actions">
                                <button
                                    onClick={(e) => {
                                        e.stopPropagation();
                                        p.onOpenScan(h.scan_id);
                                    }}
                                >
                                    Open
                                </button>
                                <button
                                    onClick={(e) => {
                                        e.stopPropagation();
                                        handleCompare(h.scan_id);
                                    }}
                                >
                                    {compareLabel(h.scan_id)}
                                </button>
                            </div>
                        </div>
                    )}
                </For>
            </Show>
        </Modal>
    );
};
