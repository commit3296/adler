/// Barrel export for the `ui/` component library. Consumers import
/// everything from here so the underlying file layout stays a
/// refactoring detail.
///
/// ```ts
/// import { Button, Modal, SearchInput, Tabs } from "@/ui";
/// ```

export { Button, type ButtonProps, type ButtonSize, type ButtonVariant } from "./primitives/Button";
export { IconButton, type IconButtonProps } from "./primitives/IconButton";
export { Input, type InputProps } from "./primitives/Input";
export { SearchInput, type SearchInputProps } from "./primitives/SearchInput";
export { Chip, type ChipProps, type ChipVariant } from "./primitives/Chip";
export { Tabs, type TabsProps, type TabsOption } from "./primitives/Tabs";
export { Modal, type ModalProps } from "./primitives/Modal";
export { Toast, type ToastProps, type ToastKind } from "./primitives/Toast";
export { Kbd, type KbdProps } from "./primitives/Kbd";
export { Icon, type IconProps } from "./primitives/Icon";
