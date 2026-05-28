import type { Component } from "solid-js";
import { actions, store, type GroupBy, type Sort } from "../store";
import { Icon, IconButton, SearchInput, Tabs } from "../ui";

interface Props {
    onExport: (kind: "json" | "csv" | "urls") => void;
}

const SORTS: { value: Sort; label: string }[] = [
    { value: "status", label: "Status" },
    { value: "name", label: "Name" },
    { value: "time", label: "Time" },
];

const GROUPS: { value: GroupBy; label: string }[] = [
    { value: "category", label: "Group" },
    { value: "none", label: "Flat" },
];

export const ResultsToolbar: Component<Props> = (p) => {
    function showExportMenu(e: MouseEvent) {
        e.stopPropagation();
        const btn = e.currentTarget as HTMLElement;
        const rect = btn.getBoundingClientRect();
        const menu = document.createElement("div");
        menu.className = "menu";
        menu.style.top = `${rect.bottom + 4}px`;
        menu.style.right = `${window.innerWidth - rect.right}px`;
        const items: { label: string; act: () => void }[] = [
            { label: "Copy URLs", act: () => p.onExport("urls") },
            { label: "Download JSON", act: () => p.onExport("json") },
            { label: "Download CSV", act: () => p.onExport("csv") },
        ];
        for (const it of items) {
            const b = document.createElement("button");
            b.className = "menu-item";
            b.textContent = it.label;
            b.addEventListener("click", () => {
                it.act();
                menu.remove();
            });
            menu.appendChild(b);
        }
        document.body.appendChild(menu);
        setTimeout(
            () =>
                document.addEventListener("click", () => menu.remove(), { once: true }),
            0,
        );
    }

    return (
        <div class="results-toolbar">
            <Tabs<Sort>
                label="Sort by"
                value={store.view.sort}
                onChange={actions.setSort}
                options={SORTS}
            />
            <Tabs<GroupBy>
                label="Group by"
                value={store.view.groupBy}
                onChange={actions.setGroupBy}
                options={GROUPS}
            />
            <SearchInput
                placeholder="Search results"
                value={store.view.resultsFilter}
                onInput={actions.setResultsFilter}
            />
            <IconButton
                title="Export (x)"
                label="Export"
                onClick={showExportMenu}
                disabled={!store.scan || store.scan.status !== "finished"}
            >
                <Icon name="download" />
            </IconButton>
        </div>
    );
};
