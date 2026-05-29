import { For, Show, type Component } from "solid-js";
import { store, type BatchEntry } from "../store";

interface Props {
    onOpen: (scanId: string) => void;
}

function titleFor(e: BatchEntry): string {
    switch (e.status) {
        case "queued":
            return "Queued";
        case "running":
            return "Scanning…";
        case "done":
            return `${e.found} found`;
        case "error":
            return "Scan failed";
    }
}

/// Progress overview for a multi-username batch: one chip per username
/// showing queued / scanning / done(+found) / error. Chips become
/// clickable once the whole batch finishes — navigating to a result
/// mid-run would close the live SSE of the in-flight scan and stall
/// the queue.
export const BatchStrip: Component<Props> = (p) => {
    const total = () => store.batch?.entries.length ?? 0;
    const done = () =>
        store.batch?.entries.filter(
            (e) => e.status === "done" || e.status === "error",
        ).length ?? 0;
    const navigable = () => !!store.batch && !store.batch.running;

    return (
        <Show when={store.batch}>
            <div class="batch-strip" role="list" aria-label="Batch progress">
                <span class="batch-strip-label">
                    Batch{" "}
                    <span class="bs-count">
                        {done()}/{total()}
                    </span>
                </span>
                <div class="batch-chips">
                    <For each={store.batch!.entries}>
                        {(e) => (
                            <button
                                type="button"
                                role="listitem"
                                class={`batch-chip ${e.status} ${
                                    e.scanId && store.scan?.id === e.scanId
                                        ? "active"
                                        : ""
                                }`}
                                disabled={!navigable() || !e.scanId}
                                title={titleFor(e)}
                                onClick={() => e.scanId && p.onOpen(e.scanId)}
                            >
                                <span class="bc-user">@{e.username}</span>
                                <Show when={e.status === "done"}>
                                    <span class="bc-found">{e.found}</span>
                                </Show>
                                <Show when={e.status === "running"}>
                                    <span class="bc-spin" aria-hidden="true" />
                                </Show>
                                <Show when={e.status === "error"}>
                                    <span class="bc-err" aria-hidden="true">
                                        !
                                    </span>
                                </Show>
                            </button>
                        )}
                    </For>
                </div>
            </div>
        </Show>
    );
};
