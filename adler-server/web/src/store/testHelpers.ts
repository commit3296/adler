import { createStore } from "solid-js/store";

import type {
    AppStore,
    BatchState,
    DiffState,
    FilterState,
    NotFoundState,
    ScanState,
    UiState,
    ViewState,
} from "../store";
import type { AccessResponse, ScanListEntry, SiteSummary } from "../types";

interface StoreOverrides {
    catalog?: SiteSummary[];
    tagsBySite?: Record<string, string[]>;
    categoryBySite?: Record<string, string>;
    tagCounts?: Record<string, number>;
    filter?: Partial<FilterState>;
    scan?: ScanState | null;
    diff?: DiffState | null;
    notFound?: NotFoundState | null;
    loading?: boolean;
    serverVersion?: string | null;
    batch?: BatchState | null;
    history?: ScanListEntry[];
    accessConfig?: AccessResponse | null;
    view?: Partial<ViewState>;
    ui?: Partial<UiState>;
}

export function createTestStore(overrides: StoreOverrides = {}) {
    return createStore<AppStore>({
        catalog: overrides.catalog ?? [],
        tagsBySite: overrides.tagsBySite ?? {},
        categoryBySite: overrides.categoryBySite ?? {},
        tagCounts: overrides.tagCounts ?? {},
        filter: {
            presetId: "quick",
            tag: [],
            excludeTag: ["bot-protected"],
            top: null,
            nsfw: false,
            egressNames: [],
            ...overrides.filter,
        },
        scan: overrides.scan ?? null,
        diff: overrides.diff ?? null,
        notFound: overrides.notFound ?? null,
        loading: overrides.loading ?? false,
        serverVersion: overrides.serverVersion ?? null,
        batch: overrides.batch ?? null,
        history: overrides.history ?? [],
        accessConfig: overrides.accessConfig ?? null,
        view: {
            sort: "status",
            groupBy: "category",
            resultsFilter: "",
            showNotFound: false,
            selectedSite: null,
            ...overrides.view,
        },
        ui: {
            drawerOpen: false,
            filtersOpen: false,
            shortcutsOpen: false,
            aboutOpen: false,
            accessOpen: false,
            comparePickerOpen: false,
            toast: null,
            compareArmed: null,
            retrying: {},
            ...overrides.ui,
        },
    });
}
