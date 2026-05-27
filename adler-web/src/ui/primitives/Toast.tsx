import { Show, type Component } from "solid-js";

export type ToastKind = "info" | "success" | "error";

export interface ToastProps {
    /** Text content. `null` / empty hides the toast — no DOM cost. */
    text: string | null;
    /** Visual treatment. `info` (default) is neutral, `success` adds
     *  a green border, `error` adds an amber border. */
    kind?: ToastKind;
}

/// Bottom-right ephemeral notification. The caller controls how long
/// it stays visible (typically 2-3s) by clearing `text` from state.
///
/// ```tsx
/// <Toast text={message()} kind="success" />
/// ```
export const Toast: Component<ToastProps> = (p) => (
    <Show when={p.text}>
        <div
            class={[
                "ui-toast",
                p.kind === "success" ? "ui-toast--success" : "",
                p.kind === "error" ? "ui-toast--error" : "",
            ]
                .filter(Boolean)
                .join(" ")}
            role="status"
            aria-live="polite"
        >
            {p.text}
        </div>
    </Show>
);
