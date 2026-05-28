import { Show, type Component, type JSX } from "solid-js";
import { Icon } from "./Icon";

export interface ModalProps {
    /** Controls visibility. Returns `null` when closed — no DOM cost. */
    open: boolean;
    /** Called on backdrop click and Escape key (caller wires Escape). */
    onClose: () => void;
    /** Title rendered in the header. Accepts JSX for badges / counts. */
    title: JSX.Element;
    /** Modal body. Caller owns layout — primitive doesn't impose
     *  padding beyond the standard `--space-5`. */
    children: JSX.Element;
    /** Optional content rendered in the footer (typically buttons). */
    footer?: JSX.Element;
    /** Optional content rendered immediately under the header
     *  (e.g. a search input). Sits inside its own border-bottom strip. */
    headerSlot?: JSX.Element;
    /** Override max-width on the inner panel. Defaults to `36rem`. */
    maxWidth?: string;
    /** Override the close-button `aria-label` / title. */
    closeLabel?: string;
    class?: string;
}

/// Centred dialog with backdrop. The `<X>` close button is always
/// rendered in the header; backdrop click also closes.
///
/// ```tsx
/// <Modal open={open()} onClose={() => setOpen(false)} title="Filters">
///   …body…
///   <ModalFooter>...buttons...</ModalFooter>
/// </Modal>
/// ```
export const Modal: Component<ModalProps> = (p) => (
    <Show when={p.open}>
        <div
            class="ui-modal-backdrop"
            onClick={(e) => {
                if (e.target === e.currentTarget) p.onClose();
            }}
        >
            <div
                class={["ui-modal", p.class ?? ""].filter(Boolean).join(" ")}
                role="dialog"
                aria-modal="true"
                style={p.maxWidth ? { "max-width": p.maxWidth } : undefined}
            >
                <header class="ui-modal__header">
                    <h2 class="ui-modal__title">{p.title}</h2>
                    <button
                        class="ui-modal__close"
                        title={p.closeLabel ?? "Close (Esc)"}
                        aria-label={p.closeLabel ?? "Close"}
                        onClick={() => p.onClose()}
                    >
                        <Icon name="close" style={{ width: "14px", height: "14px" }} />
                    </button>
                </header>
                <Show when={p.headerSlot}>{p.headerSlot}</Show>
                <div class="ui-modal__body">{p.children}</div>
                <Show when={p.footer}>
                    <footer class="ui-modal__footer">{p.footer}</footer>
                </Show>
            </div>
        </div>
    </Show>
);
