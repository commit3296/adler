import { Show, type Component, type JSX } from "solid-js";

export type ChipVariant = "include" | "exclude";

export interface ChipProps {
    /** `include` — accent-tinted pill (default). `exclude` — muted
     *  pill prefixed with a "−" glyph. */
    variant?: ChipVariant;
    /** When provided, renders a `×` dismiss control on the right
     *  that calls this handler. */
    onDismiss?: () => void;
    /** Tooltip / aria label for the dismiss control. */
    dismissTitle?: string;
    /** Read-only / muted appearance. The dismiss control still
     *  renders so the user can see the affordance was there, but
     *  clicking it does nothing. */
    disabled?: boolean;
    children: JSX.Element;
    class?: string;
}

/// Small removable pill. Used for active filter chips, current scope
/// summary in the scan header, etc.
///
/// ```tsx
/// <Chip onDismiss={() => clear()}>social</Chip>
/// <Chip variant="exclude" onDismiss={() => clear()}>bot-protected</Chip>
/// ```
export const Chip: Component<ChipProps> = (p) => (
    <span
        class={[
            "ui-chip",
            p.variant === "exclude" ? "ui-chip--exclude" : "",
            p.disabled ? "ui-chip--disabled" : "",
            p.class ?? "",
        ]
            .filter(Boolean)
            .join(" ")}
        aria-disabled={p.disabled || undefined}
    >
        {p.children}
        <Show when={p.onDismiss}>
            <span
                class="ui-chip__dismiss"
                title={p.dismissTitle ?? "Remove"}
                role="button"
                tabindex={p.disabled ? -1 : 0}
                onClick={(e) => {
                    e.stopPropagation();
                    if (p.disabled) return;
                    p.onDismiss!();
                }}
                onKeyDown={(e) => {
                    if (p.disabled) return;
                    if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        p.onDismiss!();
                    }
                }}
            >
                ×
            </span>
        </Show>
    </span>
);
