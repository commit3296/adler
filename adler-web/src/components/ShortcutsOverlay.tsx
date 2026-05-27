import type { Component } from "solid-js";
import { actions, store } from "../store";
import { Kbd, Modal } from "../ui";

export const ShortcutsOverlay: Component = () => (
    <Modal
        open={store.ui.shortcutsOpen}
        onClose={() => actions.setShortcuts(false)}
        title="Keyboard shortcuts"
        maxWidth="28rem"
    >
        <div class="shortcuts-grid">
            <dl>
                <dt>
                    <Kbd>/</Kbd>
                </dt>
                <dd>Focus search</dd>
                <dt>
                    <Kbd>Enter</Kbd>
                </dt>
                <dd>Start scan</dd>
                <dt>
                    <Kbd>Esc</Kbd>
                </dt>
                <dd>Stop / close dialog</dd>
                <dt>
                    <Kbd>j</Kbd>
                    <Kbd>k</Kbd>
                </dt>
                <dd>Move selection</dd>
                <dt>
                    <Kbd>o</Kbd>
                </dt>
                <dd>Open selected URL</dd>
                <dt>
                    <Kbd>c</Kbd>
                </dt>
                <dd>Copy URL</dd>
                <dt>
                    <Kbd>h</Kbd>
                </dt>
                <dd>Toggle history</dd>
                <dt>
                    <Kbd>f</Kbd>
                </dt>
                <dd>Open filters</dd>
                <dt>
                    <Kbd>n</Kbd>
                </dt>
                <dd>Show / hide NotFound</dd>
                <dt>
                    <Kbd>?</Kbd>
                </dt>
                <dd>This dialog</dd>
            </dl>
        </div>
    </Modal>
);
