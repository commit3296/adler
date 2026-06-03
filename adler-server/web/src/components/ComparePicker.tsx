import { For, Show, createMemo, type Component } from "solid-js";
import { actions, store } from "../store";
import { Modal } from "../ui";
import { fmtAgo, fmtElapsed } from "../lib/format";
import type { ScanListEntry } from "../types";

interface Props {
    onPick: (scanId: string) => void;
}

/// Modal that lists every finished scan for the *current scan's
/// username* (other than the current one) and lets the operator pick
/// which one to diff against. Replaces the previous auto-pick-newest
/// behaviour — when there's only one other scan, the result is the
/// same; when there are several, the operator now chooses.
///
/// Sorting: newest first. The first row is labelled "Most recent" so
/// the default-equivalent option keeps a visible anchor — same one
/// the auto-pick used.
export const ComparePicker: Component<Props> = (p) => {
    const candidates = createMemo<ScanListEntry[]>(() => {
        const cur = store.scan;
        if (!cur || cur.status !== "finished") return [];
        return store.history
            .filter(
                (h) =>
                    h.username === cur.username &&
                    h.scan_id !== cur.id &&
                    h.status === "finished",
            )
            .slice()
            .sort((a, b) => b.started_at_ms - a.started_at_ms);
    });

    function pick(id: string) {
        p.onPick(id);
        actions.setComparePicker(false);
    }

    return (
        <Modal
            open={store.ui.comparePickerOpen}
            onClose={() => actions.setComparePicker(false)}
            title="Compare with previous"
            maxWidth="32rem"
        >
            <div class="compare-picker">
                <p class="compare-picker__intro">
                    Pick a previous finished scan of{" "}
                    <code>{store.scan?.username}</code> to diff against the
                    current one. The newest is selected by default if you press
                    Enter.
                </p>
                <Show
                    when={candidates().length > 0}
                    fallback={
                        <p class="compare-picker__empty">
                            No other finished scans of this username yet.
                        </p>
                    }
                >
                    <ul class="compare-picker__list">
                        <For each={candidates()}>
                            {(entry, i) => (
                                <li>
                                    <button
                                        type="button"
                                        class={`compare-picker__row ${
                                            i() === 0 ? "compare-picker__row--default" : ""
                                        }`}
                                        onClick={() => pick(entry.scan_id)}
                                        autofocus={i() === 0}
                                    >
                                        <span class="compare-picker__when">
                                            {i() === 0 ? "Most recent" : fmtAgo(entry.started_at_ms)}
                                        </span>
                                        <span class="compare-picker__meta">
                                            {entry.summary?.found ?? 0} found ·{" "}
                                            {entry.site_count} sites ·{" "}
                                            {fmtElapsed(entry.elapsed_ms)}
                                        </span>
                                        <span class="compare-picker__ts">
                                            {new Date(entry.started_at_ms).toLocaleString()}
                                        </span>
                                    </button>
                                </li>
                            )}
                        </For>
                    </ul>
                </Show>
            </div>
        </Modal>
    );
};
