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
- `base/` ApmElement and other base classes. `app/` shell, route table, router, theme. `util/` pure
  helpers.
- `api/` `client.ts` (fetch wrapper), `errors.ts` (`ApiError`), `sse.ts` (EventSource wrapper),
  `generated/` (DTO types).
- `stores/` signals, one file per store. `components/` shared reusable components.
- `tools/` one declarative `ToolConfig` per tool. `test/` `helpers.ts`, `fake-event-source.ts`.

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

## App shell and layout contract
`index.html` holds one element, `<apm-app>` (`src/app/apm-app.ts`). It starts every app-wide service
(theme, nav/URL sync, volumes, the SSE connection, the router) and renders the single toast host:
never start those anywhere else.
- **The page itself never scrolls.** Chain: shell root `h-screen w-screen overflow-hidden grid-rows-1`
  → `drawer-content flex min-h-0 min-w-0 flex-col` → top bar and connection banner (`flex-none`) and
  `<main class="min-h-0 flex-1 overflow-hidden">`. The router renders each page as
  `<tag class="block h-full min-h-0">` inside `<main>`: a page is exactly the viewport minus the top bar.
- **Pages own their scrolling.** Lay a page out with flex/grid and give every flex or grid child that
  must shrink `min-h-0` (`min-w-0` sideways). Only a pane body (or its virtualizer) gets `overflow-auto`.
  One ancestor without `min-h-0` lets content outgrow the viewport and `overflow-hidden` clips it:
  check that first when a list does not scroll.
- The shell top bar is app-level only (page heading, `<apm-task-indicator>`, theme toggle). A tool's own
  controls (filters, split buttons, sort) live in the tool view, in its own bar.
- The shell owns the only `<h1>` (the route `label`, also the document title); views start at `<h2>`.
- A shell-level component that another feature provides (`apm-task-indicator`) gets ONE side-effect
  import line in the marked block at the top of `apm-app.ts`. Its tag is already in the top bar and
  renders nothing until it is registered.
- The menu is built from the route table; the current page gets `menu-active` and
  `aria-current="page"`. Below the `lg` breakpoint it is a DaisyUI drawer (menu button, overlay,
  Escape, closes on navigation, focus returns to the menu button).
- A DaisyUI `alert` (`role="alert"`) shows while `events.connection` is `reconnecting` or `closed`
  (not during the first `connecting`).
- The "Re-index" menu entries call `requestReindex('current' | 'all')` (`@/app/app-actions`). The
  index-controls feature registers `onReindex((scope) => ...)` (returns an unsubscribe function);
  until then the user sees an info toast. The request is a cancelable `apm-reindex` event on `document`.
- `main.ts` loads `urlpattern-polyfill` only when the browser has no `URLPattern` (the router needs
  it), then imports the shell.

## Adding a page (route or tool)
`src/app/routes.ts` is the whole route table, one entry per page in menu order: `/browse`,
`/duplicates`, `/move`, `/format`, `/cleanup` (menu group `tools`), `/trash`, `/settings` (`footer`).
`/` redirects to `/browse`; any other address renders the `notFound` page.
```ts
interface AppRoute {
  path: string; // '/duplicates', no trailing slash
  tag: string; // the custom element that renders the page
  load: () => Promise<unknown>; // imports the module that defines `tag`
  label: string; // menu text, page <h1> and document title
  icon: IconName; // from the apm-icon registry
  menu?: 'tools' | 'footer'; // omit for a page without a menu entry
  props?: Readonly<Record<string, unknown>>; // assigned to the element before it connects
}
```
Every page starts as a `comingSoon(...)` line that renders `apm-route-placeholder`. Replace only YOUR line:
```ts
{ path: '/duplicates', tag: 'apm-duplicates-page', label: 'Duplicate finder', icon: 'copy',
  load: () => import('@/tools/duplicates/apm-duplicates-page'), menu: 'tools' },
```
- `load()` runs on the first visit only: one chunk per page, only the shell loads at startup. A failed
  import shows an alert with a Retry button and is tried again on the next visit. The module
  self-registers its element like any component and imports the components it renders.
- A page receives no arguments: it reads `@/stores/nav` and the other stores itself.
- In-app links are plain anchors, `<a href=${linkTo('/trash')}>`, handled by the router (Back/Forward
  too). A link to a server resource (download, file) needs `download`, `target` or `rel="external"`,
  or the router takes it for a page. `navigate(path)` is for code: `navigate(linkTo('/move'))`.
- Tests: mount the page itself with `mount('apm-x', props)`. Route-level tests mount the shell with
  stub loaders: `mount('apm-app', { routes: [...] })` (see `stubRoute` in `app/apm-app.test.ts`).

## Stores (`src/stores/`)
A store is one module of signals plus the functions that change them (no classes). Read `.value` in
`render()` (ApmElement re-renders on change) or in `computed`/`effect`; `.peek()` reads without
subscribing.
- Values are immutable: assign a NEW object or array (`sig.value = { ...sig.value, a: 1 }`); mutating in
  place notifies nobody. State only the store may change is exported as a `ReadonlySignal` next to the
  functions that change it.
- `persistedSignal(key, fallback, validate)` (`stores/persisted.ts`) is a `Signal` kept in
  `localStorage['apm.v1.' + key]` as JSON. Stored data is untrusted: a value that fails `validate`
  falls back. It never throws (blocked or full storage keeps it in memory), does not write the initial
  value and syncs across tabs. Validators: `isBoolean`, `isString`, `oneOf(list)`,
  `numberBetween(min, max)`, `recordOf(keys, guard)`, `dictionaryOf(guard)`. For a breaking change of a
  stored shape use a new key (`public/theme-init.js` reads `apm.v1.theme` too).
- `prefs`: `theme` (`'system' | 'light' | 'dark'`), `gridSize` per pane (`setGridSize(pane, px)`, bounds
  in `GRID_SIZE`), `hideUnmatchedFolders` (hide vs grey out), `lastAction` per tool
  (`lastActionOf(tool)`, `setLastAction(tool, id)`), `sort` per pane (`SortSpec`
  `{ key: 'name' | 'date' | 'size', order: 'asc' | 'desc' }`, `setSort(pane, spec)`).
- `nav`: the tool is the path (`currentPath`, `currentTool`). Each pane (`'source' | 'comparison'`) has
  its own `locations[pane]: Signal<PaneLocation>`, `{ volume: string | null, dir: string }`. `volume` is
  the volume NAME, not its id (`volumeIdByName(name)` translates); `dir` is relative to the volume root
  (`''` = the root). Change a pane only with `setLocation(pane, location, { replace? })`: it also writes
  the URL, `?src=<volume>[/<dir>]&cmp=<volume>[/<dir>]` with literal slashes, so every address is a deep
  link and Back/Forward restore the panes. `linkTo(path)` returns `path` plus the current pane query
  (call it in `render()`). **Never unicode-normalise `dir`**: the index keeps the on-disk bytes (NFD
  names from macOS) and must match them exactly.
- `volumes`: `volumes`, `volumesStatus` (`idle | loading | ready | error`), `volumesError`,
  `volumeByName(name)`, `loadVolumes()` (never rejects). The shell loads it and refreshes it after an
  SSE reconnect and when the indexer state changes.
- `toasts`: state behind `toast.*` (see Shared components). Use the `toast` facade, not the store.

## API client (`@/api/client`)
`api.get/post/put/delete<T>(path, options?)`. `path` is the full `/api/...` path; `options` is
`{ query, body, signal, headers }`.
- `query` skips `null`/`undefined` and repeats array keys; `body` is sent as JSON. It resolves with the
  parsed JSON (`undefined` for 204). Non-GET requests carry `x-apm-request: 1` automatically.
- Failures reject with `ApiError { status, code, message, requestId? }` (`isApiError`). The server's
  `{ error: { code, message } }` is kept (`requestId` is the `x-request-id` header); a network failure is
  `status 0, code 'network'`; a non-JSON error body is `'http_error'`; an unreadable 2xx body is
  `'invalid_response'`. `errorMessage(error, fallback?)` gives user-facing text for anything thrown.
- Cancel stale requests with `signal`. An aborted call rejects with the platform `AbortError`: ignore it
  with `if (isAbortError(error)) return;` and never show a toast for it.
- Each feature area adds thin typed wrappers in `src/api/{area}.ts`, typed with the generated DTOs:
  `export const listX = (signal?: AbortSignal) => api.get<XDto[]>('/api/x', { signal });`.
  Components never call `fetch`. Thumbnails, previews and downloads are plain URLs
  (`<img src>`, `<a href download>`), not client calls.
- Tests: `vi.spyOn(api, 'get').mockResolvedValue(...)`; `client.test.ts` stubs `fetch` for the wire format.

## Live events (`@/api/sse`)
One shared `events` client; the shell starts it, so never call `events.start()` yourself.
- `events.on('task' | 'fs_change' | 'index', handler)` returns an unsubscribe function; the handler gets
  the narrowed generated `ServerEvent` variant. Subscribe in `connectedCallback`, unsubscribe in
  `disconnectedCallback`.
- `events.onResync(handler)` returns an unsubscribe function. It runs after every connection that
  follows a failure or a stop. Events are not replayed, so refetch whatever you cache there.
- `events.connection` is a signal: `connecting | open | reconnecting | closed`.
- Malformed messages are dropped with a console warning. Reconnects use capped exponential backoff with
  jitter (3 s minimum, the server's `retry:`; 30 s cap) and retry at once on `online` and when the tab
  becomes visible.
- Tests: `vi.stubGlobal('EventSource', FakeEventSource)` (`@/test/fake-event-source`: `last()`, `open()`,
  `message(data)`, `error()`, `reset()`), or `events.emit(event)` to deliver an event without a stream.
  `createEventsClient({ factory, random })` builds an isolated client. `events` is a singleton: call
  `events.stop()` in `afterEach` (a restarted client begins as `reconnecting`, which shows the banner).

## Shared components
- `apm-icon`: `<apm-icon name="trash-2" size="sm">` (`xs | sm | md | lg | xl`, takes the text colour,
  decorative unless you set `label`). `name` is the typed `IconName` union of the Lucide icons registered
  in `components/apm-icon.ts`: add a named import from `lucide` and one alphabetical line there for a new
  icon. An icon-only button still needs its own `aria-label`.
- Toasts: `toast.success/info/warning/error(message, { timeout? })` from `@/components/toast` returns an
  id for `toast.dismiss(id)`. Defaults: 4 s success and info, 6 s warning, 10 s error, `timeout: 0`
  stays until dismissed. A repeated message does not stack and at most 5 show. The shell renders the ONE
  `<apm-toast-host>`: never add another. Toasts sit below an open modal, so report a failure inside a
  modal as an inline DaisyUI `alert` in the modal.
- `apm-modal`: native `<dialog class="modal">`. Properties: `open` (reflected), `heading` (never
  `title`), `body` and `actions` (Lit templates or text; there are no slots), `size`
  (`sm | md | lg | xl | full`), `persistent` (no Esc/backdrop/X, so the content must offer a way out),
  `describedBy`. Methods `show()` and `close()`. Focus returns to the opener when it closes. It fires
  ONE non-bubbling `close` event whenever `open` goes true → false, also when you closed it yourself,
  and that event can arrive after you already moved on (opened it for the next item): in `@close`, reset
  your own state and check that the event still concerns what is on show.
  ```ts
  html`<apm-modal .open=${this.editing} heading="Rename" .body=${body} .actions=${actions}
    @close=${() => (this.editing = false)}></apm-modal>`;
  ```
- `confirmDialog({ title, message, confirmLabel = 'Confirm', cancelLabel = 'Cancel', danger = false })`
  (`@/components/confirm-dialog`) resolves `true` for the confirm button only; Esc, backdrop, X and
  Cancel resolve `false`. One shared dialog is mounted lazily and concurrent calls queue. Name the
  action in `confirmLabel` ("Delete 12 files") and put counts and sizes in `message`.
- `apm-route-placeholder`: DaisyUI hero (`heading`, `message`, `icon`, `actionHref`, `actionLabel`) for
  pages that do not exist yet.
- New component checklist: file `apm-x.ts` with tag `apm-x` and class `ApmX`; extends `ApmElement`;
  side-effect imports of every component it renders (`apm-icon` included); `customElements.define` and the
  `HTMLElementTagNameMap` entry; complete Tailwind class names; an accessible name on every control; a
  colocated test using `mount`. A page also gets its route line.

## Theme
`theme` (in `prefs`) is `'system' | 'light' | 'dark'`. `public/theme-init.js`, a classic blocking script
in the `<head>` of `index.html`, sets `<html data-theme>` from `localStorage['apm.v1.theme']` before the
first paint (no attribute for `system`: DaisyUI follows the OS). `startTheme()` (the shell calls it) keeps
the attribute in step afterwards, also across tabs. `<apm-theme-toggle>` is a three-state DaisyUI `swap`
in the shell top bar. Change the preference (`theme.value = 'dark'`); never set `data-theme` yourself.

## Accessibility minimums
- Every control has an accessible name: visible text, or `aria-label` on an icon-only button
  (`<apm-icon>` is `aria-hidden`). An input needs a `<label>` (DaisyUI `label`/`fieldset`) or `aria-label`.
- Use native elements: `<button type="button">` for actions, `<a href>` for navigation, `<dialog>` for
  overlays, `<input type="checkbox|range|radio">` for toggles and sliders. Never a click handler on a
  `div`.
- Everything works from the keyboard: Tab/Shift+Tab, Enter/Space, arrow keys in lists and menus, Escape
  closes overlays. The DOM order is the focus order: no positive `tabindex`. Keep the visible focus
  ring; never `outline-none` without a replacement.
- An overlay moves focus in when it opens and returns it to the opener when it closes (`apm-modal` and
  the shell drawer do).
- Announce changes: `role="alert"` for errors and banners, `role="status"` or `aria-live="polite"` for
  progress and counts (toasts already do). Colour is never the only signal; thumbnails get `alt` text.

## Content Security Policy
The server sends `default-src 'self'; script-src 'self'; style-src 'self'; style-src-attr
'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self'; font-src
'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'`. So: no inline
`<script>` or `<style>` elements, no `eval`/`new Function`/string timers, no `javascript:` URLs, no
`on*` attributes (use Lit `@click`), no CDN, remote font or image, no cross-origin request. Inline
`style="..."` attributes (CSS variables, Lit `styleMap`) are fine. Code that must run before the app
(the theme) is a classic file in `public/` linked from `index.html`. `src/csp.test.ts` fails on a
violation in `index.html` or `src/**`.

## Test notes
- Vitest does not reset mocks or globals between tests: a file that stubs anything ends with
  `afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); })`. Call `vi.useFakeTimers()` AFTER
  `mount`, and `vi.useRealTimers()` in `afterEach`.
- Stores are module singletons: reset what you touch in `afterEach` (`theme.value = 'system'`,
  `clearToasts()`, `localStorage.clear()`, `window.history.replaceState(null, '', '/')`). To test a
  store's first load use `vi.resetModules()` and a dynamic `import()` (see `prefs.test.ts`).
- happy-dom does no layout: assert the layout contract through classes (`min-h-0`, `overflow-hidden`),
  as `app/apm-app.test.ts` does. `app/apm-app.test.ts` is also the reference for a full shell test
  (stubbed `EventSource`, stubbed `api.get`, stub route loaders).
