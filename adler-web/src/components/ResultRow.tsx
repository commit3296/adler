import { For, Show, createSignal, type Component } from "solid-js";
import { actions, store } from "../store";
import type { CheckOutcome } from "../types";
import { reasonTag } from "../types";
import { displayUrl } from "../lib/format";
import { Icon } from "../ui";

interface Props {
    outcome: CheckOutcome;
}

export const ResultRow: Component<Props> = (props) => {
    const [expanded, setExpanded] = createSignal(false);
    const tags = (): string[] => store.tagsBySite[props.outcome.site] ?? [];
    const cleaned = (): string => displayUrl(props.outcome.url);
    const isRetrying = (): boolean => !!store.ui.retrying[props.outcome.site];
    const canRetry = (): boolean =>
        !!store.scan &&
        store.scan.status !== "running" &&
        (props.outcome.kind === "uncertain" || props.outcome.kind === "not_found");

    function metaText(): string {
        const parts = [`${props.outcome.elapsed_ms}ms`];
        const reason = reasonTag(props.outcome.reason);
        if (reason) parts.push(reason);
        return parts.join(" · ");
    }

    return (
        <div
            class={`result-row ${props.outcome.kind === "found" ? "found" : ""} ${
                props.outcome.kind === "uncertain" ? "uncertain" : ""
            } ${expanded() ? "expanded" : ""}`}
            data-site={props.outcome.site}
            tabIndex="0"
            title="Click to view details"
            onClick={(e) => {
                if ((e.target as HTMLElement).closest("a, button")) return;
                setExpanded(!expanded());
            }}
        >
            <div class="dot" />
            <div class="site">
                <span class="site-name">{props.outcome.site}</span>
                <For each={tags().slice(0, 2)}>
                    {(t) => (
                        <span
                            class="tag-pill"
                            onClick={(e) => {
                                e.stopPropagation();
                                if (!store.filter.tag.includes(t)) actions.toggleTag(t);
                                actions.toast(`Filter added: ${t}`, "success");
                            }}
                        >
                            {t}
                        </span>
                    )}
                </For>
            </div>
            <div class="url-cell">
                <a href={cleaned()} target="_blank" rel="noopener">
                    {cleaned()}
                </a>
                <CopyButton url={cleaned()} />
                <Show when={canRetry()}>
                    <button
                        type="button"
                        class={`retry-btn ${isRetrying() ? "spinning" : ""}`}
                        title="Re-probe this site"
                        aria-label="Retry this site"
                        disabled={isRetrying()}
                        onClick={(e) => {
                            e.stopPropagation();
                            actions.retrySite(props.outcome.site);
                        }}
                    >
                        <Icon name="refresh" />
                    </button>
                </Show>
            </div>
            <div class="meta-cell">
                <span>{metaText()}</span>
                <span
                    class="row-chevron"
                    title={expanded() ? "Collapse" : "Expand for details"}
                    aria-hidden="true"
                    aria-expanded={expanded()}
                >
                    <Icon name="chevron-down" />
                </span>
            </div>
            <Show when={expanded()}>
                <div class="evidence">
                    <Show
                        when={
                            props.outcome.evidence && props.outcome.evidence.length > 0
                        }
                        fallback={
                            <Show
                                when={reasonTag(props.outcome.reason)}
                                fallback={
                                    <span class="evidence-dim">No additional details.</span>
                                }
                            >
                                <span class="evidence-label">Reason</span>{" "}
                                <span class="evidence-dim">
                                    {reasonTag(props.outcome.reason)}
                                </span>
                            </Show>
                        }
                    >
                        <span class="evidence-label">Evidence</span>{" "}
                        <span class="evidence-dim">
                            {props.outcome.evidence!.map((e) => `· ${e}`).join("   ")}
                        </span>
                    </Show>
                </div>
            </Show>
        </div>
    );
};

const CopyButton: Component<{ url: string }> = (p) => {
    const [copied, setCopied] = createSignal(false);
    return (
        <button
            class={`copy-btn ${copied() ? "copied" : ""}`}
            type="button"
            onClick={async (e) => {
                e.stopPropagation();
                try {
                    await navigator.clipboard.writeText(p.url);
                    setCopied(true);
                    setTimeout(() => setCopied(false), 1100);
                } catch {
                    actions.toast("Copy failed (clipboard blocked)", "error");
                }
            }}
        >
            {copied() ? "Copied" : "Copy"}
        </button>
    );
};
