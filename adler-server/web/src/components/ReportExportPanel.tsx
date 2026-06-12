import { For, Show, createMemo, type Component } from "solid-js";

import { api, type ReportFormat } from "../api";
import { store } from "../store";
import { Icon } from "../ui";

const FORMATS: Array<{
    format: ReportFormat;
    label: string;
    ext: string;
}> = [
    { format: "json", label: "JSON", ext: "json" },
    { format: "markdown", label: "Markdown", ext: "md" },
    { format: "html", label: "HTML", ext: "html" },
];

export const ReportExportPanel: Component = () => {
    const scan = createMemo(() =>
        store.scan?.status === "finished" ? store.scan : null,
    );

    function filename(format: (typeof FORMATS)[number]): string {
        const current = scan();
        if (!current) return `adler-report.${format.ext}`;
        const base = `${current.username}-${current.id}`.replace(
            /[^A-Za-z0-9._-]+/g,
            "_",
        );
        return `adler-${base}-report.${format.ext}`;
    }

    return (
        <Show when={scan()}>
            {(current) => (
                <section
                    class="report-export-panel"
                    data-testid="report-export-panel"
                    aria-label="Investigation report exports"
                >
                    <span class="report-export-title">Case file</span>
                    <div class="report-export-actions">
                        <For each={FORMATS}>
                            {(format) => (
                                <a
                                    class="report-export-link"
                                    href={api.reportUrl(current().id, format.format)}
                                    download={filename(format)}
                                >
                                    <Icon name="download" />
                                    <span>{format.label}</span>
                                </a>
                            )}
                        </For>
                    </div>
                </section>
            )}
        </Show>
    );
};
