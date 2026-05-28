import { For, type Component } from "solid-js";

/// Placeholder shown in the scan-view shell while a scan/diff is being
/// fetched and there's nothing to render yet. Mirrors the eventual
/// layout (header + rows) so the transition to real content doesn't
/// shift the page.
export const ScanSkeleton: Component = () => (
    <div class="scan-skeleton" aria-busy="true" aria-label="Loading">
        <div class="sk-header">
            <div class="sk-line sk-title" />
            <div class="sk-line sk-sub" />
        </div>
        <For each={Array.from({ length: 8 })}>
            {() => (
                <div class="sk-row">
                    <div class="sk-dot" />
                    <div class="sk-line sk-name" />
                    <div class="sk-line sk-url" />
                    <div class="sk-line sk-meta" />
                </div>
            )}
        </For>
    </div>
);
