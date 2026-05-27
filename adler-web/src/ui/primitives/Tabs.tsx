import { For, type Component } from "solid-js";

export interface TabsOption<T extends string> {
    value: T;
    label: string;
}

export interface TabsProps<T extends string> {
    options: TabsOption<T>[];
    value: T;
    onChange: (v: T) => void;
    class?: string;
    /** Accessible label for the segmented control as a whole. */
    label?: string;
}

/// Segmented control / tab group. Generic over the option-value type
/// so consumers get exhaustive type-checking on `onChange`.
///
/// ```tsx
/// <Tabs<"status" | "name" | "time">
///   value={sort()}
///   onChange={setSort}
///   options={[
///     { value: "status", label: "Status" },
///     { value: "name",   label: "Name"   },
///     { value: "time",   label: "Time"   },
///   ]}
/// />
/// ```
export const Tabs = <T extends string>(p: TabsProps<T>): ReturnType<Component> => (
    <div
        class={["ui-tabs", p.class ?? ""].filter(Boolean).join(" ")}
        role="tablist"
        aria-label={p.label}
    >
        <For each={p.options}>
            {(opt) => (
                <button
                    type="button"
                    role="tab"
                    class="ui-tab"
                    aria-selected={p.value === opt.value}
                    onClick={() => p.onChange(opt.value)}
                >
                    {opt.label}
                </button>
            )}
        </For>
    </div>
);
