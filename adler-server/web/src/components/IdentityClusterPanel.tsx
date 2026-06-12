import { For, Show, createMemo, type Component } from "solid-js";

import { store } from "../store";
import { displayUrl } from "../lib/format";
import type { ClusterReason, IdentityCluster, ObservedProfile } from "../types";

export const IdentityClusterPanel: Component = () => {
    const clusters = createMemo(() =>
        store.scan?.status === "finished" ? store.scan.identityClusters : [],
    );

    return (
        <Show when={clusters().length > 0}>
            <section
                class="identity-clusters"
                data-testid="identity-cluster-panel"
                aria-label="Identity clusters"
            >
                <div class="identity-clusters-head">
                    <span class="identity-title">Clusters</span>
                    <span class="identity-count">{clusters().length}</span>
                </div>
                <div class="identity-cluster-grid">
                    <For each={clusters()}>
                        {(cluster) => <IdentityClusterCard cluster={cluster} />}
                    </For>
                </div>
            </section>
        </Show>
    );
};

const IdentityClusterCard: Component<{ cluster: IdentityCluster }> = (props) => {
    const reasons = createMemo(() => props.cluster.reasons ?? []);
    return (
        <article class="identity-cluster-card" data-testid="identity-cluster-card">
            <div class="identity-card-top">
                <span class="identity-id">{props.cluster.id}</span>
                <span class="identity-confidence">{props.cluster.confidence}%</span>
                <Show when={props.cluster.uncertain}>
                    <span class="identity-uncertain">uncertain</span>
                </Show>
            </div>
            <Show when={reasons().length > 0}>
                <div class="identity-reasons">
                    <For each={reasons()}>
                        {(reason) => (
                            <span class="identity-reason">
                                {formatReason(reason)}
                            </span>
                        )}
                    </For>
                </div>
            </Show>
            <div class="identity-members">
                <For each={props.cluster.members}>
                    {(member) => <IdentityMember member={member} />}
                </For>
            </div>
        </article>
    );
};

const IdentityMember: Component<{ member: ObservedProfile }> = (props) => {
    const url = createMemo(() => displayUrl(props.member.url));
    return (
        <a
            class="identity-member"
            href={url()}
            target="_blank"
            rel="noopener"
            title={url()}
        >
            <span class="identity-member-site">{props.member.site}</span>
            <span class="identity-member-url">{url()}</span>
        </a>
    );
};

function formatReason(reason: ClusterReason): string {
    switch (reason.kind) {
        case "shared_display_name":
            return `display name: ${reason.value}`;
        case "shared_bio_phrase":
            return `bio phrase: ${reason.phrase}`;
        case "shared_external_link":
            return `external link: ${reason.value}`;
        case "shared_location":
            return `location: ${reason.value}`;
        case "shared_avatar_url":
            return `avatar URL: ${reason.value}`;
        case "historical_co_occurrence":
            return "historical co-occurrence";
    }
}
