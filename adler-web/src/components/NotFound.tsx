import { Show, type Component } from "solid-js";
import type { NotFoundState } from "../store";

interface Props {
    nf: NotFoundState;
    onHome: () => void;
}

/// Full-view "this doesn't exist" state. Replaces the previous
/// behaviour where a bad `#/scan/:id` left an empty scan-view shell
/// behind a transient error toast.
export const NotFound: Component<Props> = (p) => {
    const heading = () => {
        switch (p.nf.kind) {
            case "scan":
                return "Scan not found";
            case "diff":
                return "Comparison not found";
            default:
                return "Page not found";
        }
    };
    const detail = () => {
        switch (p.nf.kind) {
            case "scan":
                return "No persisted scan with this id. History keeps the most recent 200 scans — older ones are pruned.";
            case "diff":
                return "One or both of the scans in this comparison no longer exist.";
            default:
                return "That address doesn't match any view in Adler.";
        }
    };
    return (
        <section class="not-found">
            <div class="nf-inner">
                <div class="nf-code">404</div>
                <h1 class="nf-heading">{heading()}</h1>
                <p class="nf-detail">{detail()}</p>
                <Show when={p.nf.detail}>
                    <code class="nf-ref">{p.nf.detail}</code>
                </Show>
                <button class="primary-btn" onClick={() => p.onHome()}>
                    Back to search
                </button>
            </div>
        </section>
    );
};
