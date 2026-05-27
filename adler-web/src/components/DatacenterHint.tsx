import { Show, createMemo, type Component } from "solid-js";
import { store } from "../store";
import { Icon } from "../ui";

/// Shown above the results when a finished scan reveals that the
/// current network can't verify bot-protected sites — typically the
/// case from a datacenter / VPS IP. The hint suggests residential
/// proxy / FlareSolverr / Browserbase options.
///
/// Threshold: > 50% of probed bot-protected sites returned Uncertain
/// AND at least 3 such sites were probed (avoids false positives on
/// tiny filters).
export const DatacenterHint: Component = () => {
    const stats = createMemo<
        { total: number; uncertain: number; ratio: number } | null
    >(() => {
        const scan = store.scan;
        if (!scan || scan.status !== "finished") return null;
        let total = 0;
        let uncertain = 0;
        for (const o of scan.outcomes) {
            const tags = store.tagsBySite[o.site] ?? [];
            if (!tags.includes("bot-protected")) continue;
            total++;
            if (o.kind === "uncertain") uncertain++;
        }
        if (total < 3) return null;
        const ratio = uncertain / total;
        if (ratio <= 0.5) return null;
        return { total, uncertain, ratio };
    });

    return (
        <Show when={stats()}>
            <div class="dc-banner" role="note">
                <Icon name="warning" />
                <div class="dc-banner-body">
                    <div class="dc-banner-title">
                        {stats()!.uncertain} of {stats()!.total} bot-protected sites
                        couldn't be verified from this network.
                    </div>
                    <div class="dc-banner-detail">
                        Datacenter / VPS IPs are blocked by Instagram, X/Twitter,
                        Facebook, TikTok and similar. To verify them, route the
                        scanner through one of:
                    </div>
                    <ul class="dc-banner-list">
                        <li>
                            a residential proxy:{" "}
                            <code>adler --proxy socks5://host:port …</code>
                        </li>
                        <li>
                            a self-hosted FlareSolverr:{" "}
                            <code>adler --flaresolverr http://localhost:8191 …</code>
                        </li>
                        <li>
                            Browserbase (paid):{" "}
                            <code>adler --browser-backend browserbase …</code>
                        </li>
                    </ul>
                </div>
            </div>
        </Show>
    );
};
