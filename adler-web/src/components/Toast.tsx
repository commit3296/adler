import { Show, type Component } from "solid-js";
import { actions, store } from "../store";

export const Toast: Component = () => (
    <Show when={store.ui.toast}>
        <div
            class={[
                "ui-toast",
                store.ui.toast!.kind === "success" ? "ui-toast--success" : "",
                store.ui.toast!.kind === "error" ? "ui-toast--error" : "",
            ]
                .filter(Boolean)
                .join(" ")}
            role="status"
            aria-live="polite"
            title="Click to dismiss"
            onClick={() => actions.setToast(null)}
        >
            {store.ui.toast!.text}
        </div>
    </Show>
);
