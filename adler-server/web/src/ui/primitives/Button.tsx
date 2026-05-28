import { splitProps, type Component, type JSX } from "solid-js";

export type ButtonVariant = "primary" | "secondary" | "ghost";
export type ButtonSize = "sm" | "md" | "lg";

export interface ButtonProps
    extends Omit<JSX.ButtonHTMLAttributes<HTMLButtonElement>, "size"> {
    /** Visual emphasis. `primary` for the page's single most-important
     *  CTA (Scan, Apply). `secondary` for confirmations and dialog
     *  primaries. `ghost` for tertiary inline actions (Rescan, Stop). */
    variant?: ButtonVariant;
    /** Compact (`sm`) for inline / toolbar buttons, default (`md`)
     *  for dialog footers, `lg` for the hero CTA. */
    size?: ButtonSize;
    /** Tints a `ghost` button red on hover — for destructive actions
     *  like Stop or Cancel. No effect on other variants. */
    danger?: boolean;
}

/// Single-purpose action button.
///
/// ```tsx
/// <Button variant="primary" onClick={...}>Scan</Button>
/// <Button variant="ghost" size="sm" danger>Stop</Button>
/// ```
export const Button: Component<ButtonProps> = (props) => {
    const [own, rest] = splitProps(props, [
        "variant",
        "size",
        "danger",
        "class",
        "children",
        "type",
    ]);
    const variant = () => own.variant ?? "primary";
    const size = () => own.size ?? "md";
    return (
        <button
            type={own.type ?? "button"}
            class={[
                "ui-btn",
                `ui-btn--${variant()}`,
                `ui-btn--${size()}`,
                own.danger ? "ui-btn--danger" : "",
                own.class ?? "",
            ]
                .filter(Boolean)
                .join(" ")}
            {...rest}
        >
            {own.children}
        </button>
    );
};
