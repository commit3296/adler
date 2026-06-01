import { Show, type Component } from "solid-js";
import { actions, store } from "../store";
import { Icon, IconButton } from "../ui";

export const TopBar: Component = () => {
    function home() {
        actions.clearBatch();
        actions.clearScan();
        location.hash = "#/";
    }
    return (
        <header class="top-bar">
            <a
                class="logo-link"
                href="#/"
                onClick={(e) => {
                    e.preventDefault();
                    home();
                }}
            >
                ADLER
            </a>
            <div class="top-actions">
                <IconButton
                    title="History (h)"
                    label="History"
                    active={store.ui.drawerOpen}
                    onClick={() => actions.setDrawer(!store.ui.drawerOpen)}
                >
                    <Icon name="clock" />
                    <Show when={store.history.length > 0}>
                        <span class="topbar-badge">{store.history.length}</span>
                    </Show>
                </IconButton>
                <IconButton
                    title="Filters (f)"
                    label="Filters"
                    active={store.ui.filtersOpen}
                    onClick={() => actions.setFilters(!store.ui.filtersOpen)}
                >
                    <Icon name="filter" />
                </IconButton>
                <IconButton
                    title="Shortcuts (?)"
                    active={store.ui.shortcutsOpen}
                    onClick={() => actions.setShortcuts(!store.ui.shortcutsOpen)}
                >
                    <Icon name="help" />
                </IconButton>
                <IconButton
                    title="Access engine"
                    label="Access"
                    active={store.ui.accessOpen}
                    onClick={() => actions.setAccess(!store.ui.accessOpen)}
                >
                    <Icon name="shield" />
                </IconButton>
                <IconButton
                    title="About"
                    active={store.ui.aboutOpen}
                    onClick={() => actions.setAbout(!store.ui.aboutOpen)}
                >
                    <Icon name="info" />
                </IconButton>
            </div>
        </header>
    );
};
