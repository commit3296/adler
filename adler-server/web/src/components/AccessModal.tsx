import {
    Show,
    createEffect,
    createMemo,
    createSignal,
    type Component,
} from "solid-js";
import { Modal } from "../ui";
import { actions, store } from "../store";
import { api } from "../api";
import type { AccessResponse } from "../types";

/// Read-only view of the access engine's runtime config —
/// `--proxy-pool` entries (country + kind, *not* proxy URLs) and
/// `--sessions` names (no header values). Editing happens by updating
/// the TOML files and restarting the server; we surface them in the
/// SPA so the operator can confirm what's loaded without shell
/// access. Sensitive material (proxy credentials, session cookies)
/// never reaches this side.
export const AccessModal: Component = () => {
    const [data, setData] = createSignal<AccessResponse | null>(null);
    const [loading, setLoading] = createSignal(false);
    const [error, setError] = createSignal<string | null>(null);

    async function refresh() {
        setLoading(true);
        setError(null);
        try {
            const resp = await api.access();
            setData(resp);
        } catch (e) {
            setError(e instanceof Error ? e.message : String(e));
        } finally {
            setLoading(false);
        }
    }

    // Re-fetch whenever the modal transitions to open.
    createEffect(() => {
        if (store.ui.accessOpen) {
            void refresh();
        }
    });

    const hasEgress = createMemo(() => (data()?.egress.length ?? 0) > 0);
    const hasSessions = createMemo(() => (data()?.sessions.length ?? 0) > 0);

    return (
        <Modal
            open={store.ui.accessOpen}
            onClose={() => actions.setAccess(false)}
            title="Access engine"
            maxWidth="36rem"
        >
            <div class="access-body">
                <p class="access-intro">
                    What the server currently has loaded from{" "}
                    <code>--proxy-pool</code> and <code>--sessions</code>. Read-
                    only: edit the source files and restart the server to
                    apply changes — sensitive material (proxy credentials,
                    session cookies / tokens) is deliberately kept off this
                    HTTP API so it never reaches the browser.
                </p>

                <Show when={error()}>
                    <p class="access-empty" role="alert">
                        Failed to load access config: {error()}
                    </p>
                </Show>

                <section class="access-section">
                    <h3>Egress pool</h3>
                    <Show
                        when={!loading()}
                        fallback={<p class="access-loading">Loading…</p>}
                    >
                        <Show
                            when={hasEgress()}
                            fallback={
                                <p class="access-empty">
                                    No proxy pool configured. Start the server
                                    with{" "}
                                    <code>
                                        adler --web --proxy-pool pool.toml
                                    </code>{" "}
                                    to route per-site egress.
                                </p>
                            }
                        >
                            <table class="access-table">
                                <thead>
                                    <tr>
                                        <th>Country</th>
                                        <th>Kind</th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {(data()?.egress ?? []).map((e) => (
                                        <tr>
                                            <td>{e.country ?? "—"}</td>
                                            <td>
                                                <span
                                                    class={`access-kind kind-${e.kind}`}
                                                >
                                                    {e.kind}
                                                </span>
                                            </td>
                                        </tr>
                                    ))}
                                </tbody>
                            </table>
                        </Show>
                    </Show>
                </section>

                <section class="access-section">
                    <h3>Sessions</h3>
                    <Show
                        when={!loading()}
                        fallback={<p class="access-loading">Loading…</p>}
                    >
                        <Show
                            when={hasSessions()}
                            fallback={
                                <p class="access-empty">
                                    No sessions configured. Start the server
                                    with{" "}
                                    <code>
                                        adler --web --sessions sessions.toml
                                    </code>{" "}
                                    to reach login-walled sites.
                                </p>
                            }
                        >
                            <ul class="access-sessions">
                                {(data()?.sessions ?? []).map((s) => (
                                    <li>
                                        <code>{s.name}</code>
                                    </li>
                                ))}
                            </ul>
                        </Show>
                    </Show>
                </section>

                <p class="access-foot">
                    <button
                        type="button"
                        class="access-refresh"
                        onClick={() => void refresh()}
                    >
                        Refresh
                    </button>
                </p>
            </div>
        </Modal>
    );
};
