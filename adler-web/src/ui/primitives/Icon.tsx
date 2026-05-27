import type { Component, JSX } from "solid-js";

export interface IconProps {
    /** Sprite symbol id without the `icon-` prefix.
     *  e.g. `"search"` → `<use href="#icon-search"/>`. */
    name: string;
    /** Optional style override (size, color). Color inherits from
     *  `currentColor` by default so parent state drives the icon. */
    style?: JSX.CSSProperties;
}

/// Render a sprite icon. The sprite itself (`<IconSprite/>`) must be
/// mounted once at the app root. Stroke colour is `currentColor`, so
/// hover / active / disabled states on the parent button flow
/// through automatically.
export const Icon: Component<IconProps> = (p) => (
    <svg class="icon" style={p.style}>
        <use href={`#icon-${p.name}`} />
    </svg>
);
