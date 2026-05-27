import { splitProps, type Component, type JSX } from "solid-js";

export interface InputProps extends JSX.InputHTMLAttributes<HTMLInputElement> {}

/// Bordered text input with focus ring. Use for form-style fields
/// (advanced filters, settings). For inline filter affordances with a
/// search icon, reach for `<SearchInput>` instead.
export const Input: Component<InputProps> = (props) => {
    const [own, rest] = splitProps(props, ["class", "type"]);
    return (
        <input
            type={own.type ?? "text"}
            class={["ui-input", own.class ?? ""].filter(Boolean).join(" ")}
            {...rest}
        />
    );
};
