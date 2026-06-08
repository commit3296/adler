import { createEffect } from "solid-js";

export function useDocumentTitle(title: () => string): void {
    createEffect(() => {
        document.title = title();
    });
}
