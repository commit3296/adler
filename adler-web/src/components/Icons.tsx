// Inline SVG icon sprite. Mounted once at the top of <App/> so any
// `<Icon name="..."/>` (from the ui/ library) resolves via
// `<use href="#icon-foo"/>`. Add new symbols here; the `Icon` wrapper
// component lives in `ui/primitives/Icon.tsx`.

import type { Component } from "solid-js";

export const IconSprite: Component = () => (
    <svg width="0" height="0" style={{ position: "absolute" }} aria-hidden="true">
        <defs>
            <symbol id="icon-search" viewBox="0 0 24 24">
                <circle cx="11" cy="11" r="7" />
                <line x1="20" y1="20" x2="16.65" y2="16.65" />
            </symbol>
            <symbol id="icon-clock" viewBox="0 0 24 24">
                <circle cx="12" cy="12" r="9" />
                <polyline points="12 7 12 12 15 14" />
            </symbol>
            <symbol id="icon-filter" viewBox="0 0 24 24">
                <polygon points="22 3 2 3 10 12.46 10 19 14 21 14 12.46 22 3" />
            </symbol>
            <symbol id="icon-help" viewBox="0 0 24 24">
                <circle cx="12" cy="12" r="9" />
                <path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3" />
                <line x1="12" y1="17" x2="12.01" y2="17" />
            </symbol>
            <symbol id="icon-download" viewBox="0 0 24 24">
                <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                <polyline points="7 10 12 15 17 10" />
                <line x1="12" y1="15" x2="12" y2="3" />
            </symbol>
            <symbol id="icon-close" viewBox="0 0 24 24">
                <line x1="18" y1="6" x2="6" y2="18" />
                <line x1="6" y1="6" x2="18" y2="18" />
            </symbol>
            <symbol id="icon-refresh" viewBox="0 0 24 24">
                <polyline points="23 4 23 10 17 10" />
                <path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10" />
            </symbol>
            <symbol id="icon-chevron-down" viewBox="0 0 24 24">
                <polyline points="6 9 12 15 18 9" />
            </symbol>
            <symbol id="icon-diff" viewBox="0 0 24 24">
                <polyline points="22 12 16 12 14 15 10 9 8 12 2 12" />
            </symbol>
            <symbol id="icon-warning" viewBox="0 0 24 24">
                <path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" />
                <line x1="12" y1="9" x2="12" y2="13" />
                <line x1="12" y1="17" x2="12.01" y2="17" />
            </symbol>
        </defs>
    </svg>
);
