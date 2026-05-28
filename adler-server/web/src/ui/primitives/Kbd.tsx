import type { Component, JSX } from "solid-js";

export interface KbdProps {
    children: JSX.Element;
}

/// Inline `<kbd>` styled to match the rest of the UI. Use inside
/// help text, tooltips, shortcut tables.
export const Kbd: Component<KbdProps> = (p) => (
    <kbd class="ui-kbd">{p.children}</kbd>
);
