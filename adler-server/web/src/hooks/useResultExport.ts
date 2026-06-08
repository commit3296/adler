import { displayUrl } from "../lib/format";
import { actions, store } from "../store";

export type ExportKind = "json" | "csv" | "urls";

export function useResultExport(): (kind: ExportKind) => void {
    return (kind: ExportKind) => {
        if (!store.scan) return;
        if (kind === "urls") {
            const urls = store.scan.outcomes
                .filter((o) => o.kind === "found")
                .map((o) => displayUrl(o.url));
            if (urls.length === 0) {
                actions.toast("No found URLs to copy", "error");
                return;
            }
            navigator.clipboard
                .writeText(urls.join("\n"))
                .then(() => actions.toast(`Copied ${urls.length} URLs`, "success"))
                .catch(() => actions.toast("Copy failed", "error"));
            return;
        }

        const filename = `adler-${store.scan.username}-${store.scan.id}.${kind}`;
        let body = "";
        let type = "";
        if (kind === "json") {
            body = JSON.stringify(
                {
                    username: store.scan.username,
                    scan_id: store.scan.id,
                    summary: store.scan.summary,
                    outcomes: store.scan.outcomes,
                },
                null,
                2,
            );
            type = "application/json";
        } else {
            const lines = ["site,kind,url,elapsed_ms,reason"];
            const csvEscape = (v: string) =>
                /[",\n]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v;
            for (const o of store.scan.outcomes) {
                const reason =
                    o.reason === undefined
                        ? ""
                        : typeof o.reason === "string"
                          ? o.reason
                          : Object.keys(o.reason)[0]!;
                lines.push(
                    [
                        csvEscape(o.site),
                        csvEscape(o.kind),
                        csvEscape(o.url),
                        String(o.elapsed_ms),
                        csvEscape(reason),
                    ].join(","),
                );
            }
            body = lines.join("\n");
            type = "text/csv";
        }

        const blob = new Blob([body], { type });
        const a = document.createElement("a");
        a.href = URL.createObjectURL(blob);
        a.download = filename;
        document.body.appendChild(a);
        a.click();
        a.remove();
        setTimeout(() => URL.revokeObjectURL(a.href), 5000);
    };
}
