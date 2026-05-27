# `adler-web/src/ui/` — component library

Self-contained SolidJS + TypeScript component library. The whole `ui/`
folder is intentionally portable: drop it into any other SolidJS app
(with the matching `tokens.css`) and the primitives work unchanged.

```
ui/
├── README.md            ← you are here
├── tokens.css           CSS variables (colors, spacing, radius, …)
├── primitives/
│   ├── Button.tsx       primary | secondary | ghost  ×  sm | md | lg
│   ├── IconButton.tsx   square button, icon-only, with active state
│   ├── Input.tsx        bordered text input with focus ring
│   ├── SearchInput.tsx  Input composed with a search icon
│   ├── Chip.tsx         small pill, optional × dismiss, exclude variant
│   ├── Tabs.tsx         segmented control / tab group
│   ├── Modal.tsx        backdrop + centred dialog (header / body / footer)
│   ├── Toast.tsx        bottom-right ephemeral notification
│   ├── Kbd.tsx          inline `<kbd>` styled
│   └── Icon.tsx         <use href="#icon-name"/> wrapper (sprite-based)
└── index.ts             barrel — `import { Button, Modal } from "./ui"`
```

## Principles

1. **Tokens, not values.**  Components never inline colours / radii /
   sizes. They reference `var(--…)` from `tokens.css`. A re-theme of
   the whole library is "edit `tokens.css`".

2. **Primitives over one-offs.**  If a UI need can be expressed by an
   existing primitive (possibly with a new variant), add a variant;
   don't make a fresh component. If it _can't_, the primitive is
   probably missing — extract it once two consumers need the same shape.

3. **Composition over inheritance.**  Composed components (e.g.
   `SearchInput = Input + Icon`) live next to primitives but build
   _from_ them, not in parallel. No primitive duplicates another's
   focus ring or border.

4. **Class passthrough.**  Every primitive accepts an optional `class`
   prop, appended to its own classes. Parents own layout
   (`flex: 1`, margins, grid position) — primitives own appearance.

5. **No layout assumptions.**  A primitive should look right whether
   it sits in a flex row, grid cell, or block flow. No global
   margins, no `width: 100%`, no implicit positioning.

6. **Accessible by default.**  Visible focus ring (red 2px outline,
   1px offset). Buttons and inputs receive keyboard focus naturally.
   `aria-*` props are exposed where they apply (`aria-modal`,
   `aria-hidden`, `aria-pressed`).

7. **No external CSS frameworks.**  No Tailwind, no styled-components.
   Plain CSS + variables. The whole library ships as one CSS file
   plus the TSX components.

8. **Variants are exhaustive enums.**  TypeScript discriminated
   unions where possible (e.g. `variant: "primary" | "secondary" |
   "ghost"`). Reviewer can grep all call sites.

## Token reference (excerpt — full list in `tokens.css`)

```
--color-bg            #000             base background
--color-bg-elev       #0c0c0c          one-step elevated (toolbar, modal)
--color-bg-elev-2     #141414          two-step elevated (hover, modal body)
--color-bg-row-hover  #181818          row hover background

--color-fg            #f0f0f0          primary text (WCAG AAA on bg)
--color-fg-dim        #bcbcbc          secondary (WCAG AA)
--color-fg-faint      #7e7e7e          tertiary / decorative only

--color-line          #1f1f1f          subtle divider
--color-line-strong   #303030          visible border

--color-accent        #ff2d2d          primary CTA / brand
--color-accent-text   #ff5757          accent text on dark-accent surfaces
--color-accent-bg     #240a0a          dark-accent surface (chip bg)
--color-success       #3dd06f          found / done
--color-success-text  #5bdf85
--color-warning       #f4b14c          uncertain / warning
--color-warning-text  #f7c373

--radius              2px              sharp by design, NOT rounded
--font-mono           ui-monospace, …
--font-sans           ui-sans-serif, …
```

The existing short tokens (`--bg`, `--fg`, `--red`, …) remain as
aliases for backwards-compatibility with the styles.css that lives
outside `ui/`.  New code uses the long names; old code migrates as
it's touched.

## When to use what

| Need | Primitive |
|---|---|
| Primary action (Scan, Apply, Save) | `<Button variant="primary">` |
| Secondary action (Rescan, Reset) | `<Button variant="secondary">` |
| Tertiary (Stop, low-emphasis) | `<Button variant="ghost">` |
| Square icon-only (header, row hover) | `<IconButton>` |
| Form field (free text) | `<Input>` |
| Filter / search field | `<SearchInput>` |
| Small removable label | `<Chip>` |
| Pill with `×` to remove | `<Chip onDismiss>` |
| Segmented control (Sort / Group) | `<Tabs>` |
| Centred dialog (filters, history, shortcuts) | `<Modal>` |
| Ephemeral notification | `<Toast>` |
| Inline keyboard key | `<Kbd>` |
| Vector icon from sprite | `<Icon name="…">` |

## Adding a new primitive

1. Drop it under `primitives/`.
2. Re-export from `index.ts`.
3. Document props with JSDoc — that's the contract for consumers.
4. Add the role to the table above.
5. If it needs a new token, add it to `tokens.css` *and* the
   reference table here.

## Portability

The folder has zero runtime dependencies beyond `solid-js`. To copy
into another SolidJS project:

1. Copy `ui/` over.
2. Include `ui/tokens.css` (or merge its `:root` block into your
   project's globals).
3. Mount the SVG sprite from `Icon.tsx` once at the app root.

That's it.
