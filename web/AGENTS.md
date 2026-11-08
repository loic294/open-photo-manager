# APM web: rules for coding agents

Lit 3 + DaisyUI 5 / Tailwind 4, Vite 8, TypeScript 7 (strict), Biome, Vitest + happy-dom.
State is signals (`@preact/signals-core`), routing `@lit-labs/router`, big lists `@lit-labs/virtualizer`.
Work from `web/`. `src/api/generated/` is written by the Rust build: never edit it.
Agents work in parallel worktrees: touch only your component's files and tests, and leave shared
files (`package.json`, configs, `src/base/`) alone unless your task says so.

## Commands
- `npm run dev`: Vite on :5173. Proxies `/api` to 127.0.0.1:8080 and keeps the browser's Host header
  (the backend's CSRF check compares Origin to Host). Several worktrees can run a full stack side by
  side on their own ports: `APM_LISTEN=127.0.0.1:18080 make dev-server` and
  `APM_API_TARGET=http://127.0.0.1:18080 npm --prefix web run dev -- --port 15173 --strictPort`
  (any `localhost` port passes the Host check). Never reuse 8080/5173 from a worktree.
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
  helpers. `controllers/` reusable Lit `ReactiveController`s.
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
(theme, nav/URL sync, volumes, tasks, the SSE connection, the router) and renders the single toast host:
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
- A shell-level component that another feature provides gets ONE side-effect import line in the marked
  block at the top of `apm-app.ts`; its tag can already be in the template and renders nothing until it
  is registered. `apm-task-indicator` was added that way (see Task indicator).
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
  SSE reconnect and whenever an `index` task changes state (its first event included, progress-only
  updates not).
- `toasts`: state behind `toast.*` (see Shared components). Use the `toast` facade, not the store.
- `settings`: the server-wide settings document (`settings`, `loadSettings()`, `saveSettings(doc)`); see
  Settings below.
- `tasks`: the server's background tasks (`tasks`, `aggregate`, `cancel(id)`, `dismiss(id)`); the shell
  starts it and the top bar shows it. See Task indicator below.

## API client (`@/api/client`)
`api.get/post/put/delete<T>(path, options?)`. `path` is the full `/api/...` path; `options` is
`{ query, body, signal, headers }`.
- `query` skips `null`/`undefined` and repeats array keys; `body` is sent as JSON. It resolves with the
  parsed JSON (`undefined` for 204). Non-GET requests carry `x-apm-request: 1` automatically.
- Failures reject with `ApiError { status, code, message, fields, requestId? }` (`isApiError`). The server's
  `{ error: { code, message, fields? } }` is kept (`requestId` is the `x-request-id` header; `fields` is the
  `{ field, message }[]` of a 422 `validation_failed`, `[]` otherwise, with paths such as
  `index.concurrency` or `sidecar_extensions[2]`); a network failure is
  `status 0, code 'network'`; a non-JSON error body is `'http_error'`; an unreadable 2xx body is
  `'invalid_response'`. `errorMessage(error, fallback?)` gives user-facing text for anything thrown.
  Branch on `code`, never on the HTTP status (`invalid_json` can be a 400, 415 or 422).
- Cancel stale requests with `signal`. An aborted call rejects with the platform `AbortError`: ignore it
  with `if (isAbortError(error)) return;` and never show a toast for it.
- Each feature area adds thin typed wrappers in `src/api/{area}.ts`, typed with the generated DTOs:
  `export const listX = (signal?: AbortSignal) => api.get<XDto[]>('/api/x', { signal });`.
  Components never call `fetch`. Thumbnails, previews and downloads are plain URLs
  (`<img src>`, `<a href download>`), not client calls.
- Tests: `vi.spyOn(api, 'get').mockResolvedValue(...)`; `client.test.ts` stubs `fetch` for the wire format.

## Live events (`@/api/sse`)
One shared `events` client; the shell starts it, so never call `events.start()` yourself.
- `events.on('task' | 'fs_change', handler)` returns an unsubscribe function; the handler gets the
  narrowed generated `ServerEvent` variant. Subscribe in `connectedCallback`, unsubscribe in
  `disconnectedCallback`. These are the only two event types: indexing arrives as a `task` event
  whose `task.kind` is `'index'` (there is no separate index event).
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
- `apm-chip-input`: a labelled list of short texts edited as removable chips, with validation (see
  Settings below).
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

## Selection
One `PaneSelection` per pane, `selection.source` and `selection.comparison` (`@/stores/selection`), and
`activePane`, a signal holding the pane the user touched last (the target of the shortcuts). The set of
selected item ids survives scrolling, sorting and refetching.
- Read the read-only signals `ids` (a NEW `ReadonlySet<number>` on every change: hand it to the grid as
  is), `count`, `active` (something is selected: a plain click selects instead of opening) and `busy`
  (waiting for ids), or `has(id)` / `toArray()`. Read inside `render()` they subscribe the element.
- Change it with `toggle(id)`, `add(ids)`, `set(ids)`, `clear()` (synchronous) and `rangeTo(id)`,
  `selectAll()`, `reconcile()` (return a `Promise<void>` that never rejects). The async three ask the
  ids provider for the whole ordered list; calls in flight share one request; `clear()`, `set()` and a
  new context discard it; a failure shows one error toast (a failed `reconcile()` stays silent).
- `interpret(activation, { active })` (`@/util/selection-rules`) is the pure click table: Shift = range,
  Cmd/Ctrl = toggle, a plain click toggles while `active` and opens otherwise, Enter always opens.
- `SelectionController` (`@/controllers/selection-controller`) turns the grid's bubbling `card-activate`
  into a selection change, or into a bubbling `item-open { id, index }` event from the host (the viewer
  listens for it). It marks its pane active on `pointerdown` and `focusin`.
- `installSelectionShortcuts()` (`@/controllers/selection-shortcuts`): Cmd/Ctrl+A selects everything in
  the active pane, Escape clears the active pane, then the other. Both stand aside in text fields and
  while a `dialog[open]` exists. Arrows, Space and Enter belong to the grid.

Wiring in the pane element:
```ts
private readonly selecting = new SelectionController(this, { pane: () => this.pane });

// whenever the request changes: volume, dir and filter, NOT plan or sort
selection[this.pane].setContext(selectionContextKey(request));
// connectedCallback; `null` in disconnectedCallback. Real files only, current sort; read the
// current request when it is called, and pass `signal` to the `POST /api/browse/ids` call.
selection[this.pane].setIdsProvider((signal) => loadIds(signal));
// after an `fs_change` for which `fsChangeAffects` is true (it does nothing when empty)
void selection[this.pane].reconcile();
```
In `render()` pass `.selectedIds=${this.selecting.selectedIds} .selectionActive=${this.selecting.active}`
to the grid. Footer: `selectedCount = selecting.count`, `selecting = selecting.busy`,
`active = selecting.isActivePane`; `select-all` calls `selecting.selectAll()` and `clear-selection`
calls `selecting.clear()` (both also mark the pane active). The tool view calls
`installSelectionShortcuts()` once in `connectedCallback` and the returned function in
`disconnectedCallback`. Tests call `resetSelectionForTests()` in `afterEach`.

## Bottom bar and split button
Both (`@/components/`) are presentational: properties in, bubbling events out. Show a count to a user
with `formatCount(n)` (`@/util/format-count`, digit grouping).

`<apm-bottom-bar>` (host `block`) is the footer of a tool: per pane the counts, the selection and the
thumbnail size, at the far right the tool's actions. It keeps no state and reads no store.
- `source: PaneFooter`, `comparison?: PaneFooter | null` (none = no second segment) and
  `actions?: TemplateResult`, the far-right area (a property: there are no slots). Listeners inside an
  `actions` template must be arrow functions, because Lit calls a method with the bar as `this`.
- `PaneFooter { counts: FooterCounts | null; selectedCount; selecting; active; size; sizeMin; sizeMax;
  sizeStep }`: `counts: null` is loading (skeletons, Select all off), `selecting` is a select-all in
  flight, `active` is the pane the shortcuts act on (primary top border). Fill it from the selection.
- Events, all bubbling, with `detail.pane` (`'source' | 'comparison'`): `size-change { pane, size }` on
  EVERY slider `input` (a number, so the grid resizes live), `select-all { pane }` and
  `clear-selection { pane }`. The bar changes nothing itself: the parent updates the properties.
- Each segment is a `@container` and the bar never wraps: the counts truncate first (the full text is
  in their tooltip), the slider shrinks and is hidden below 14rem, "Select all" shows its label from
  `@lg` and the badge its word "selected" from `@sm`. The counts tooltip is `tooltip-start`: the
  shell's `<main>` is `overflow-hidden`, so a centred tooltip is cut off at the left edge.

`<apm-split-button>` (host `inline-flex`): the main button runs the SELECTED entry, the chevron opens a
menu upwards to select another one.
- `options: SplitOption[]`, `SplitOption { id; label; icon?: IconName; danger?; disabled?; hint? }`
  (`danger` = red main button while selected, `hint` = tooltip on the entry), `default-id`,
  `persist-key` (the tool id), `count` (badge in the main button), `busy` (spinner, both buttons off)
  and `disabled`.
- Events, bubbling, `detail` is `{ optionId }`: `split-action` comes ONLY from the main button,
  `split-select` from a menu entry, which selects and remembers it but never runs it. The actions are
  destructive, so the host owns the confirmation (`confirmDialog`) on `split-action`.
- The chosen entry is stored in the `lastAction` pref (`localStorage['apm.v1.lastAction']`) under
  `persist-key`; the typed `lastActionOf` needs a `ToolId`, so the element uses the signal. The entry
  shown is the remembered one if it is enabled, else `default-id`, else the first enabled one; a
  remembered entry that is disabled or gone is skipped, NOT forgotten. Without `persist-key` the
  choice lives as long as the element. With no enabled entry the first one shows and both buttons are
  off; without options nothing renders. Keyboard: Down/Up open and move, Home/End, Esc closes the menu
  only (the event is stopped, so a page Esc shortcut does not also run).
- `make check-web` prints "Found 2 warnings while optimizing generated CSS" (Invalid empty selector)
  since `join` is used: DaisyUI's z-index rules for a focused or hovered `join` child are dropped by
  the CSS optimiser. The focus ring and the hover still look right (checked in a built page): ignore it.
- Tests: `mount('apm-split-button', { options })`; reset `lastAction.value = {}` and
  `localStorage.clear()` in `afterEach`. Move a bar slider by setting `value`, then dispatch `input`.

## Settings (`src/tools/settings/`, `stores/settings.ts`)
`/settings` is `apm-settings-page`: a title, DaisyUI `tabs-border` built from `settingsSections`
(`tools/settings/sections.ts`) and the open section below. The open id lives in `?section=<id>` (written
with `replaceState`, so no history entry; the other query keys and the hash stay; Back/Forward are
followed; a missing or unknown id opens the first tab). Today there is one section, `general`
(`apm-settings-general`: three cards, "Files and sidecars", "Moving and deleting", "Indexing").
- **Add a section** (for example the Immich connection): append ONE entry to `settingsSections` and change
  nothing else, not the page, not `routes.ts`:
  ```ts
  { id: 'immich', label: 'Immich', icon: 'plug', tag: 'apm-settings-immich',
    load: () => import('@/tools/settings/apm-settings-immich') },
  ```
  The page draws the tab, imports the module the first time the tab opens (a failed import shows a Retry)
  and renders `<tag></tag>` with no properties or children. A section loads and saves its own data and
  reports its own errors. It stays in the page, hidden, while another tab is open, so edits that are not
  saved yet survive a switch of tabs: it is not disconnected, so it must not poll or hold a stream while
  `this.checkVisibility()` is false. Tests: `sections` can be injected
  (`mount('apm-settings-page', { sections })`), see `apm-settings-page.test.ts`.
- `stores/settings.ts` holds the server-wide settings document: `settings` (`ReadonlySignal<SettingsDto |
  null>`, `null` until a load or a save succeeded), `loadSettings()` (calls share one request; rejects
  with an `ApiError`) and `saveSettings(doc)` (PUTs the WHOLE document, resolves with and stores the
  document the server normalised: show that, not your draft; never run two saves at once). Other pages
  read it too, for example the trash page's `trash_retention_days`. Edit a copy, never the signal's value.
- `apm-settings-general` is the reference for a form on the server's data: a draft copy, a dirty flag,
  a sticky Save/Discard bar only while dirty, a `beforeunload` guard only while dirty (the router has no
  navigation veto), fields disabled while saving, and a 422 mapped through `ApiError.fields` to the
  field or chip it names (what it cannot place goes in an alert; other failures in a toast).
- `tools/settings/limits.ts` mirrors the server's bounds and normalisation (`SCAN_CONCURRENCY`,
  `NAME_MAX_LEN`, `normalizeExtension`, `validateJunkName`, ...) only to warn before Save. The server is
  the authority (`crates/apm-core/src/dto/settings.rs`): when they disagree, show its message.
- `apm-chip-input` (`@/components/apm-chip-input`): `label`, `hint`, `placeholder`, `max`, `.values`,
  `.normalizeEntry`, `.validateEntry`, `error`, `.errorIndexes`, `allow-spaces`, `disabled`; fires
  `values-change` (`detail.values`). It renders the whole labelled field, never mutates `values` and
  its `focus()` focuses its text box.
- Test data: `sampleSettings(overrides?)` (`@/test/settings-fixtures`) is the document of a fresh server.
- Gotchas learned here: (1) happy-dom ignores `selected` on `<option>`s that Lit renders in a first
  render: give options only a `value` and set `select.value` in `updated()`. (2) A child component
  re-enables itself in its own, later update: in the parent's `updated()` await the child's
  `updateComplete` before calling `focus()` on it. (3) Do not give a method the name of a native member
  (`remove`, `normalize`, `update`): TypeScript or the DOM will clash with it.

## File grid
`<apm-file-grid>` (`@/components/apm-file-grid`) shows one listing as cards (`<apm-file-card>`), read
page by page through `PagedItems` (`@/stores/paged-items`). Both panes use it.
```ts
html`<apm-file-grid .request=${request} sort=${sort} order=${order} size=${size}
  .selectedIds=${selecting.selectedIds} .selectionActive=${selecting.active}></apm-file-grid>`;
```
- Properties: `request` (`BrowseRequest`; `undefined` shows placeholders), `sort`, `order`, `size` (card
  width in px, clamped to `GRID_SIZE` of `@/stores/prefs`: 96 to 320, step 16, default 160),
  `.selectedIds`, `selection-active`, `empty-text`, `label`. A new `request`, `sort` or `order` starts
  a new listing (an equal `request` in a new object does not). Also `columns` (read-only, cards per
  row), `focusItem(index)` and `focus()`.
- Events, both bubbling: `card-activate` from the cards (`{ id, index, via: 'checkbox' | 'card' |
  'space' | 'enter', shift, toggle }`) and `grid-state` (`{ total, loading, error }`, when one of them
  changes; `total` is `null` until the first page and counts ghosts). The grid keeps no selection and
  opens nothing: `SelectionController` (see Selection) turns `card-activate` into `item-open`.
- A ghost (`overlay.planned`) is a file the plan will create: dashed and dimmed, listed after the real
  files, never selectable, never emitting, counted in `total` but absent from `fetchIds`.
- NOT a scroller: it is as tall as its listing and the pane body (`min-h-0 flex-1 overflow-y-auto`)
  scrolls it, with the folder list above. It scrolls only to bring a focused card into view; a new
  listing keeps the scroll position the browser clamps, so the pane resets `body.scrollTop = 0` when
  the folder, the sort or the filter changes. A failed fetch shows an `alert` with Retry (alone on a
  first page, sticky above the list otherwise).
- Up to 200 items (`VIRTUALIZE_ABOVE`) it is a plain CSS grid, above that `@lit-labs/virtualizer`
  renders only the cards near the view. `virtualize()` looks for the scrolling ancestors ONCE when it
  starts, and a Lit template renders into a detached fragment that has none, so a virtualiser started
  in the first render never follows the pane: the grid renders its host first and starts `virtualize()`
  on the next render. Keep that, and never put the grid in a shadow root. happy-dom has no layout, so
  the unit tests cover the plain grid and the pure helpers; check the virtualised path in a browser.
- Keyboard: one tab stop (roving tabindex). Arrows, Home, End, PageUp and PageDown move the focus
  (`gridFocusTarget`), Shift only moves it, Space and Enter are the card's.
- `PagedItems`: `reset(request, sort, order): boolean`, `get(index)`, `retry()`, `invalidate()`,
  `dispose()`, read-only signals `total`, `version`, `error`, `loading`. It refetches itself after a
  matching `fs_change` (debounced) or a stream resync and swaps the data in without an empty frame: a
  pane never calls `invalidate()` for that. `fsChangeAffects(scope, event, { subtree })`
  (`@/util/fs-change`) is the one "does this `fs_change` change what I show" rule: use it for the
  folder list (with `subtree`) and for `reconcile()`.
- `@/api/files`: `fetchItems`, `fetchIds` (every real file id in display order: select-all, the
  viewer), `thumbUrl(item, 'sm' | 'lg')`, `thumbSizeFor(cssPx)`. `@/util/format`: `formatCaptureDate`,
  `formatFileSize`, `formatDuration`. `cardBadges(item)` is the one place to add a card badge and
  `cardFrameClasses` documents the border precedence. `fileItem()` (`@/test/file-item`) builds a
  `FileItem` for tests.

## Task indicator (`stores/tasks.ts`, `apm-task-indicator`, `apm-task-row`)
Every background job (move, trash, index, plan execution, Immich) is a server task, and the shell top
bar shows all of them in ONE place. **A tool view needs no progress UI of its own** (no bar, no spinner,
no polling, no stream): submit the job (the request answers 202 with its `TaskDto`) and let the indicator
show it; cancel and dismiss live there too. Refresh a pane from the `fs_change` events the runners
emit, not from the task.
- `stores/tasks.ts`: the shell calls `startTasks()` once, never call it yourself. Signals: `tasks`
  (`ReadonlySignal<readonly TaskDto[]>`, the VISIBLE tasks in display order), `aggregate`
  (`{ running, queued, failed, fraction }`: `fraction` is the mean `taskFraction` of the running tasks
  that know their total, 0 to 1, `null` when none does) and `cancelling` (`ReadonlySet<number>` of ids).
  Functions: `loadTasks()`, `cancel(id)`, `dismiss(id)`, `dismissFinished()`, `onTaskFinished(handler)`
  (once per task that ends while the page is open; returns the unsubscribe function) and the pure
  `isActive`, `isEnded`, `isFailure`, `taskFraction(task)` (bytes when the task has a byte total, else
  items; `null` without a total). Constants: `FINISHED_LINGER_MS` (5000), `STALE_FAILURE_SECS` (24 h),
  `DISMISSED_LIMIT` (200).
- **Visibility.** Queued and running tasks always show. `done` and `cancelled` ones stay 5 s. `failed` and
  `interrupted` ones stay until dismissed (the trigger gets a red badge); the dismissed ids are kept in
  `localStorage['apm.v1.tasks.dismissed']` (the newest 200), so a reload does not bring them back.
  `GET /api/tasks` also lists history: the first list adds only a failure that ended less than 24 h ago
  and was not dismissed, never a finished task. Order: running (oldest start first), queued (by id),
  finished (newest first). A late event never moves a task back to an earlier state.
- `cancel(id)` never rejects: a failure is an error toast, 404 and 409 are ignored and the list is read
  again. `cancelling` keeps the id until the task's TERMINAL state arrives (not just the 202), so the row
  keeps saying "Cancelling…" without flicker: a running task stops only when its runner returns.
- The title comes from the server and changes while a task runs: show it as text (never HTML), never cache
  it, and keep it short and specific ("Move 120 files"): it is also the label in the top bar.
- `<apm-task-row .task=${task} .cancelling=${false} compact>` (host `block`) shows one task: kind icon,
  status dot, title, a bar (determinate with a total, indeterminate without, none while queued), the
  counts, speed and time left, and the error text of a failure. It reads no store and fires bubbling
  `task-cancel` and `task-dismiss` with `detail { id }`. `compact` (a narrow spot) drops the kind icon, the
  speed and the time left and tightens the spacing.
- `<apm-task-indicator>` has no properties and no events (host `contents`) and renders NOTHING while no
  task is visible, so it takes no room in the bar. Otherwise it is one button: a bar (indeterminate until
  a running task has a total; `w-16`, `md:w-28`, so it leaves room for the page heading on a phone), a
  `w-9` tabular share, the label from `md` up, a red badge with the failed count and a grey one with the
  queued count. Nothing in it changes width while a task progresses. Its accessible name says it in words
  and contains the visible text ("2 tasks running, 62 percent, 1 failed").
- The panel (one row per task and "Clear finished") opens on hover and on focus, pins on click, Enter and
  Space (that is how touch opens it) and closes on a second click, an outside press, Escape or the focus
  leaving. It is driven in JS (`dropdown-open`, `dropdown-close`), not by `dropdown-hover`: CSS alone can
  neither be dismissed with Escape nor stay pinned. While open, Escape is handled on the document in the
  capture phase and is then NOT passed on, so the page's own Escape does not also run (an open modal
  `dialog` keeps its Escape). The trigger is a disclosure button (`aria-expanded`, `aria-controls`), not a
  menu: no `aria-haspopup`. Never put a `tabindex` on a DaisyUI dropdown trigger (its `:first-child` rule
  removes the pointer events) and never use `tooltip` inside the scrolling panel (a native `title` is
  used).
- The panel is `md:w-96` hung from the right edge of the trigger (`dropdown-end`). Below `md` it is
  `max-md:fixed max-md:inset-x-2`: a sheet 0.5rem from each side, just under the trigger. A panel hung
  from the trigger would stick out of a phone screen on the left, because the theme toggle follows the
  trigger in the bar. Keep that in mind if the header grows (anything `fixed` needs an ancestor without
  a `transform`).
- ONE `aria-live="polite"` region announces only the END of a task ("Move 120 files finished", "Indexing
  failed", "… was cancelled", "… was interrupted"), never its progress, and keeps the last 5.
- `@/util/format`: `formatRate(perSecond, 'bytes' | 'items')` ("42 MB/s", "85 items/s") and
  `formatEta(seconds)` ("under 1 min", "about 3 min", "about 2 h 10 min"). `@/api/tasks`: `listTasks`,
  `cancelTask`. Icons added for the task kinds: `brush-cleaning`, `cloud-download`, `cloud-upload`,
  `list-checks`.
- Tests: `makeTask(overrides)` and `taskEvent(overrides)` (`@/test/task-fixtures`). After `startTasks()`,
  feed the store with `events.emit(taskEvent({ ... }))`; mount the element FIRST and call
  `vi.useFakeTimers()` after it; call `resetTasksForTests()` BEFORE `localStorage.clear()` in `beforeEach`
  and `afterEach`. happy-dom has no hover: dispatch `pointerenter` and `focusin` yourself.
