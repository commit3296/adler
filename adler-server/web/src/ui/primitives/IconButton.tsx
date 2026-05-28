import { splitProps, type Component, type JSX } from "solid-js";

export interface IconButtonProps
    extends Omit<JSX.ButtonHTMLAttributes<HTMLButtonElement>, "title"> {
    /** Visible label rendered next to the icon. Optional — leave out
     *  for icon-only square buttons. Auto-hides under 600px viewport. */
    label?: string;
    /** Required for accessibility — used as `title` and as the
     *  `aria-label` when there is no visible label. */
    title: string;
    /** When `true`, gives the button the "selected" treatment (red
     *  border + accent colour). Use for toggle-like buttons that pair
     *  with a dialog or drawer. */
    active?: boolean;
}

/// Header / toolbar icon button. Square by default, expands when a
/// label is provided.
///
/// ```tsx
/// <IconButton title="Open history" active={open()} onClick={...}>
///   <Icon name="clock" />
///   <span class="label">History</span>
/// </IconButton>
/// ```
export const IconButton: Component<IconButtonProps> = (props) => {
    const [own, rest] = splitProps(props, [
        "label",
        "title",
        "active",
        "class",
        "children",
        "type",
    ]);
    return (
        <button
            type={own.type ?? "button"}
            title={own.title}
            aria-label={own.label ?? own.title}
            data-active={own.active ? "true" : undefined}
            class={["ui-icon-btn", own.class ?? ""].filter(Boolean).join(" ")}
            {...rest}
        >
            {own.children}
            {own.label && <span class="label">{own.label}</span>}
        </button>
    );
};
