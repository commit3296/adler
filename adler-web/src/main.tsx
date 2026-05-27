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
