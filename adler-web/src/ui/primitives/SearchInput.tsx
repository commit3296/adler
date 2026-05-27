import { splitProps, type Component } from "solid-js";
import { Icon } from "./Icon";

export interface SearchInputProps {
    placeholder: string;
    value: string;
    onInput: (next: string) => void;
    autofocus?: boolean;
    id?: string;
    class?: string;
}

/// "Magnifier + text" filter affordance. Same focus ring + mono
/// input as the primitive `<Input>`, with the icon composed inside.
///
/// ```tsx
/// <SearchInput
///   placeholder="Search results"
///   value={query()}
///   onInput={setQuery}
/// />
/// ```
export const SearchInput: Component<SearchInputProps> = (props) => {
    const [own] = splitProps(props, [
        "placeholder",
        "value",
        "onInput",
        "autofocus",
        "id",
        "class",
    ]);
    return (
        <div class={["ui-search", own.class ?? ""].filter(Boolean).join(" ")}>
            <Icon name="search" />
            <input
                id={own.id}
                placeholder={own.placeholder}
                value={own.value}
                onInput={(e) => own.onInput(e.currentTarget.value)}
                autofocus={own.autofocus}
            />
        </div>
    );
};
