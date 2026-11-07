# APM web: rules for coding agents

Lit 3 + DaisyUI 5 / Tailwind 4, Vite 8, TypeScript 7 (strict), Biome, Vitest + happy-dom.
State is signals (`@preact/signals-core`), routing `@lit-labs/router`, big lists `@lit-labs/virtualizer`.
Work from `web/`. `src/api/generated/` is written by the Rust build: never edit it.
Agents work in parallel worktrees: touch only your component's files and tests, and leave shared
files (`package.json`, configs, `src/base/`) alone unless your task says so.

## Commands
- `npm run dev`: Vite on :5173. Proxies `/api` to 127.0.0.1:8080 and keeps the browser's Host header
  (the backend's CSRF check compares Origin to Host).
- `npm run format`: Biome auto-fix (single quotes, 2 spaces, width 100, sorted imports). Run it
  before the gate; `npm run lint` only reports.
- `npm run typecheck`, `npm test` (one file: `npx vitest run src/x/apm-y.test.ts`), `npm run build`.
- `npm run check` = typecheck + lint + test + build. It must be green before you finish.

## UI: DaisyUI first
- Build UI from DaisyUI components: btn, card, modal, menu, navbar, drawer, join, dropdown, toggle,
  range, progress, tabs, badge, list, breadcrumbs, filter, rating, table, toast, skeleton, tooltip.
  Never hand-roll something DaisyUI already provides. Class names: https://daisyui.com/components/
  (offline: `node_modules/daisyui/components/*.css`).
- Layout and spacing use plain Tailwind utilities (flex, grid, gap-4, p-2). Colours use DaisyUI
  tokens (bg-base-200, text-primary), never hex values.
- Zero custom CSS. If truly unavoidable, add it to `src/styles.css` with a comment saying why.
- Tailwind scans `src/**` and `index.html` (test files and `api/generated/` excluded). Write COMPLETE
  class names; a name built by concatenation (`btn-${kind}`) is silently dropped from the CSS.
  Choose between full names instead: `kind === 'main' ? 'btn-primary' : 'btn-ghost'`.
- Theme: `data-theme="light"` or `"dark"` on `<html>`; without it the OS preference applies.

## Components
- Every component extends `ApmElement` (`src/base/apm-element.ts`) and renders in the LIGHT DOM, so
  DaisyUI classes apply with no per-component CSS.
- Light DOM caveats: no `<slot>`; `static styles`, `:host` and `::slotted` do nothing; never write
  children inside a component tag (Lit leaves them unmanaged). Compose with properties and templates.
- Decorator-free Lit: reactive properties go in `static override properties`; no `@property` or
  `@customElement`. `useDefineForClassFields` is `false` in `tsconfig.json`, which keeps field
  initialisers reactive in Vitest and in the production build. NEVER change it (a test guards it).
- Read store signals inside `render()` to subscribe; do not copy their values into properties.
- Naming: tag `apm-photo-card`, file `apm-photo-card.ts` (same kebab-case name), class `ApmPhotoCard`,
  one component per file, test `apm-photo-card.test.ts` beside it.
- NO component barrel/index: a component side-effect-imports the components it renders.
- Reuse before creating: search `src/components` first and extend via properties; never copy one.

```ts
import { html } from 'lit';
import { ApmElement } from '@/base/apm-element';
import '@/components/apm-thumb'; // side-effect import of every component rendered below

export class ApmPhotoCard extends ApmElement {
  static override properties = { caption: { type: String }, selected: { type: Boolean } };
  declare caption: string | undefined; // no default: `declare`, never a bare `caption: string;`
  selected = false; // defaults are plain initialisers

  protected override render() {
    return html`<div class="card bg-base-100"><apm-thumb></apm-thumb>${this.caption}</div>`;
  }
}

customElements.define('apm-photo-card', ApmPhotoCard);

declare global {
  interface HTMLElementTagNameMap {
    'apm-photo-card': ApmPhotoCard;
  }
}
```

## Layout (`@/` = `src/`, also in tests)
- `base/` ApmElement and other base classes. `app/` shell and routing. `util/` pure helpers.
- `api/` `client.ts` (fetch wrapper), `sse.ts` (EventSource wrapper), `generated/` (DTO types).
- `stores/` signals, one file per store. `components/` shared reusable components.
- `tools/` one declarative `ToolConfig` per tool. `test/` `helpers.ts`.

## Types and API
- DTOs: `import type { X } from '@/api/generated/X'`: one file per type, no barrel. Never hand-write
  a duplicate. `import type` is mandatory (`verbatimModuleSyntax`).
- JSON is snake_case, ids are numbers, browse and facets are POST with a JSON body.
- Every non-GET request MUST send header `x-apm-request: 1` (CSRF guard); GET and SSE are exempt.
  Send requests through `src/api/client.ts` and streams through `src/api/sse.ts`.
- Errors look like `{ "error": { "code": "...", "message": "..." } }`.

## Testing
- Colocated `*.test.ts`, Vitest + happy-dom. `const el = await mount('apm-photo-card', { selected: true })`
  from `@/test/helpers` appends to `document.body`, awaits `updateComplete` and cleans up after each
  test. Await `child.updateComplete` yourself for nested components.
- Query the light DOM directly (`el.querySelector('.btn')`); there is no `shadowRoot`.
- In agent shells Vitest hides `console.log` of passing tests: use `npx vitest run --reporter=verbose`.
- `src/base/apm-element.test.ts` is the reference for property, signal and light-DOM tests.

## Gotchas
- Never name a property like a native `HTMLElement` member (`title`, `hidden`, `id`, `lang`, `dir`,
  `draggable`, `slot`): it clashes with the native attribute and the DOM typings. Use `caption` etc.
- A bare `setTimeout`/`setInterval` is typed `NodeJS.Timeout` (config files pull in `@types/node`):
  use `window.setTimeout` or `ReturnType<typeof setTimeout>`.
- Dependencies are pinned exactly (`.npmrc` has `save-exact=true`): no `^` or `~`. Add one only if
  the task needs it, and commit `package-lock.json`.
