// User-facing semantic categories — mapped from registry tags.
// First matching bucket wins for any given site.
export interface Category {
    id: string;
    label: string;
    tags: string[]; // empty = catch-all (must be last)
}

export const CATEGORIES: Category[] = [
    { id: "social", label: "Social", tags: ["social", "dating", "lgbt"] },
    { id: "dev", label: "Developer", tags: ["coding", "dev"] },
    {
        id: "forum",
        label: "Forums",
        tags: ["forum", "discourse", "phpbb", "xenforo", "vbulletin"],
    },
    { id: "gaming", label: "Gaming", tags: ["gaming", "esports"] },
    {
        id: "media",
        label: "Media",
        tags: ["video", "music", "photo", "art", "writing", "blogging"],
    },
    { id: "other", label: "Other", tags: [] },
];

// Curated quick-pick presets shown under the hero search input.
export interface Preset {
    id: string;
    label: string;
    filter: {
        tag?: string[];
        exclude_tag?: string[];
        top?: number;
        nsfw?: boolean;
    };
}

export const PRESETS: Preset[] = [
    { id: "quick", label: "Quick", filter: { exclude_tag: ["bot-protected"] } },
    { id: "social", label: "Social", filter: { tag: ["social", "dating", "lgbt"] } },
    { id: "dev", label: "Dev", filter: { tag: ["coding", "dev"] } },
    {
        id: "forum",
        label: "Forums",
        filter: { tag: ["forum", "discourse", "phpbb", "xenforo", "vbulletin"] },
    },
    { id: "gaming", label: "Gaming", filter: { tag: ["gaming", "esports"] } },
    { id: "all", label: "All", filter: {} },
];

export function categoryForTags(tags: string[]): Category {
    for (const cat of CATEGORIES) {
        if (cat.tags.length === 0) continue;
        if (cat.tags.some((t) => tags.includes(t))) return cat;
    }
    return CATEGORIES[CATEGORIES.length - 1]!;
}
