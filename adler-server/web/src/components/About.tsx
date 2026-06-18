import { type Component } from "solid-js";
import { Modal } from "../ui";
import { actions, store } from "../store";

const REPO = "https://github.com/commit3296/adler";

/// Lightweight "what is this" panel for first-time users. Honest about
/// what detection can and can't do — mirrors the project's
/// no-overclaiming stance.
export const About: Component = () => (
    <Modal
        open={store.ui.aboutOpen}
        onClose={() => actions.setAbout(false)}
        title="About Adler"
        maxWidth="34rem"
    >
        <div class="about-body">
            <p>
                <strong>Adler</strong> searches a username across thousands of
                sites — a modern successor to Sherlock, written in Rust and
                served from a single self-hosted binary.
            </p>
            <h3>How detection works</h3>
            <p>
                Each site is probed with <em>multi-signal detection</em>: the
                HTTP status, body markers, and redirect behaviour are combined
                into one verdict — <span class="about-found">Found</span>,{" "}
                <span class="about-nf">Not found</span>, or{" "}
                <span class="about-unc">Uncertain</span> — rather than relying
                on a single status check. This lowers false positives on sites
                that return <code>200</code> for every username.
            </p>
            <h3>Honest limits</h3>
            <p>
                Recall depends on where you scan from: bot-protected sites
                (Instagram, X, Facebook…) serve a login wall to plain requests
                and read as <span class="about-unc">Uncertain</span> without a
                browser backend. Some sites can't distinguish a real account
                from a missing one at all and are deliberately left out rather
                than guessed.
            </p>
            <p class="about-links">
                <a href={REPO} target="_blank" rel="noopener">
                    Source &amp; docs on GitHub
                </a>
                <span class="f-sep">·</span>
                <a
                    href={`${REPO}#detection-rate`}
                    target="_blank"
                    rel="noopener"
                >
                    What doesn't detect, and why
                </a>
            </p>
        </div>
    </Modal>
);
