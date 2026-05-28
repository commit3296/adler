import { Show, type Component } from "solid-js";
import { store } from "../store";

interface Props {
    onAbout: () => void;
}

const REPO = "https://github.com/commit3296/adler";

/// Persistent footer on every screen: brand + server version, an
/// About trigger, the source link, and a hint that the same data is
/// available as a JSON API for scripting.
export const Footer: Component<Props> = (p) => (
    <footer class="app-footer">
        <span class="f-brand">Adler</span>
        <Show when={store.serverVersion}>
            <span class="f-ver">v{store.serverVersion}</span>
        </Show>
        <span class="f-sep">·</span>
        <button class="f-link" type="button" onClick={() => p.onAbout()}>
            About
        </button>
        <span class="f-sep">·</span>
        <a class="f-link" href={REPO} target="_blank" rel="noopener">
            GitHub
        </a>
        <span class="f-sep">·</span>
        <span class="f-api">
            JSON API at <code>/api</code>
        </span>
    </footer>
);
