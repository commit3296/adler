/* @refresh reload */
import { render } from "solid-js/web";
import { App } from "./App";

// Order matters: design tokens first (defines `:root` variables),
// then app-specific styles that consume them.
import "./ui/tokens.css";
import "./styles.css";

const root = document.getElementById("root");
if (!root) throw new Error("missing #root element");
render(() => <App />, root);

// The app shell mounts synchronously above; fade the inline boot
// preloader out so the handoff to the rendered UI is a crossfade
// rather than a hard cut.
const boot = document.getElementById("boot-loader");
if (boot) {
    boot.style.opacity = "0";
    setTimeout(() => boot.remove(), 200);
}
