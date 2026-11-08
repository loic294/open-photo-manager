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
- `tools/` one declarative `ToolConfig` per tool, in `tools/<tool>/`; `tools/framework/` is the tool view
  they all share (see Tool view). `test/` `helpers.ts`, `fake-event-source.ts`.

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
(theme, nav/URL sync, volumes, tasks, the index status, the re-index actions, the SSE connection, the
router) and renders the single toast host: never start those anywhere else.
- **The page itself never scrolls.** Chain: shell root `h-screen w-screen overflow-hidden grid-rows-1`
  → `drawer-content flex min-h-0 min-w-0 flex-col` → top bar, connection banner and index notices
  (`flex-none`) and `<main class="min-h-0 flex-1 overflow-hidden">`. The router renders each page as
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
  (not during the first `connecting`). Right under it `<apm-index-notices>` warns about the removals the
  scanner held back (see Index controls).
- The "Re-index" menu entries call `requestReindex('current' | 'all')` (`@/app/app-actions`), and
  `startReindex()` (`@/app/reindex`, started by the shell) is the `onReindex` handler that does the work
  (see Index controls). The entries are `disabled` (`menu-disabled` on the `li`) while a request is being
  sent and while the event stream is down; the pressed one shows a spinner instead of its icon. The
  request is a cancelable `apm-reindex` event on `document`; with no handler (the shell is not mounted)
  the user sees an info toast.
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
- A tool page (`/browse`, `/duplicates`, `/move`, `/format`, `/cleanup`) renders `<apm-tool-view
  .config=${config}>` and nothing else: see Tool view.
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
- `apm-select`: the ONE drop-down of the app. Nobody writes a raw `<select>` (see Pane and folder list
  below).
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
  into a selection change, or into a bubbling `item-open { id, index }` event from the host (the tool
  view listens for it and calls `openViewer`, see Viewer). It marks its pane active on `pointerdown` and
  `focusin`.
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

### Immich settings
`apm-settings-immich` is the lazy Immich tab. Its Connection, Library mapping, Sync and Changes
waiting for Immich cards use `api/immich.ts` (generated DTOs only) and the shared pure
`describeImmichError(error)` for actionable errors, including stored sync/outbox reason codes.
- `stores/immich.ts`: `immichStatus`, `immichStatusError`, `immichAttention` are read-only signals.
  `loadImmichStatus({ check }?)` resolves on failure without losing the old status; concurrent plain
  loads share a request and explicit checks are separate. `applyImmichStatus(status)` adopts a
  connect, disconnect or mapping save immediately and invalidates older loads. The shell owns
  `startImmich(): () => void`: initial load, SSE resync and a one-second debounced reload after
  finished `immich_sync`, `immich_cleanup`, `fs_move`, `fs_trash`, `fs_rmdir`, `plan_execute` or
  `trash_empty` tasks. No polling, including while Settings is hidden.
- General and mapping forms share `apm-save-bar` (`invalid`, `saving`, `message`,
  `invalidMessage`; bubbling `save`/`discard` events) and `UnsavedGuard(host, () => dirty)`.
  Call the guard's `sync()` in `updated()`; it listens for `beforeunload` only while connected
  and dirty, and removes the listener on disconnect. Keep drafts across tab switches.
- Mapping saves send the complete set in volume row order, skipping unmapped rows. A validation
  path `mappings[i].path_prefix` refers to the i-th **sent entry**, not the visible row; retain
  that index-to-volume association. Successful saves adopt the returned mapping set before
  requesting a background sync. Detect requests are aborted/ignored on library changes or removal.
- Passwords belong only to the connect form, are cleared after success/removal, and never enter
  browser storage or a store. Test cards with `vi.spyOn(api, 'get'/'post'/'put'/'delete')`, real
  signals and `sampleImmichStatus`, `sampleLibraries`, `sampleOutboxPage` from
  `test/immich-fixtures.ts`. Await nested elements' `updateComplete` before querying their controls.

## File grid
`<apm-file-grid>` (`@/components/apm-file-grid`) shows one listing as cards (`<apm-file-card>`), read
page by page through `PagedItems` (`@/stores/paged-items`). Both panes use it.
```ts
html`<apm-file-grid .request=${request} sort=${sort} order=${order} size=${size}
  .selectedIds=${selecting.selectedIds} .selectionActive=${selecting.active}></apm-file-grid>`;
```
- Properties: `request` (`BrowseRequest`; `undefined` shows placeholders), `sort`, `order`, `size` (card
  width in px, clamped to `GRID_SIZE` of `@/stores/prefs`: 96 to 320, step 16, default 160, and to the
  grid's own width: the virtualiser renders no card in a grid narrower than one card, so a narrow pane
  shows one shrunk column), `.selectedIds`, `selection-active`, `empty-text`, `label`. A new `request`,
  `sort` or `order` starts a new listing (an equal `request` in a new object does not). Also `columns`
  (read-only, cards per row), `itemAt(index)` (the loaded item or `undefined`: the viewer's `startItem`),
  `refresh()` (fetches what is on screen again; the items stay until the answer), `focusItem(index)` and
  `focus()`.
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

## Viewer (`openViewer`, `@/components/apm-viewer`)
The photo and video modal. `openViewer(source: ViewerSource): Promise<void>` resolves when it closes. One
`apm-viewer` (an `apm-modal size="full"`) is mounted lazily on the first call. Only the tool-view
framework calls it, once per pane, on the grid's `item-open`. Pass the pane's CURRENT request and sort so
the viewer steps through what the grid shows, in its order:
```ts
this.addEventListener(ITEM_OPEN_EVENT, (event) => {
  const { id, index } = (event as ItemOpenEvent).detail; // from '@/controllers/selection-controller'
  void openViewer({
    startId: id,
    startItem: this.items.get(index), // optional: the row is shown at once, the server is not asked
    loadIds: (signal) => fetchIds(request, sort, order, { signal }), // called once; real files only
  });
});
```
- Calling it while it is open replaces the source (everything of the old one is aborted); every pending
  promise resolves once, when the viewer closes. There is no `closeViewer()`, route or shell wiring.
- If `startId` is not in the list, or `loadIds` fails, it shows that one file without stepping and says so
  in a toolbar badge. It never raises a toast.
- Keys: Left, Right, Home, End step (no wrap-around), `i` toggles the info panel, `z` the 100% zoom (images
  only), Esc closes. Keys with a modifier, keys while a `video` or `audio` has the focus, and the arrows
  before the list has arrived are left alone. For `OPEN_GUARD_MS` after opening, clicks and double clicks
  anywhere in the viewer are swallowed: the grid opens it on the first click of a double click.
- The pieces: `apm-viewer.ts` (the session: list, current file, requests, keys, focus), `viewer-chrome.ts`
  (pure toolbar, step buttons and info panel markup over a `ChromeView` and `ChromeActions`),
  `apm-viewer-stage.ts` (shows ONE file: `item`, `zoomed`, `failure`; non-bubbling `stage-settled`,
  `stage-retry`, `zoom-toggle`; `release()`), `viewer-nav.ts` (the pure key, step and preload rules, the
  labels, `DETAIL_DEBOUNCE_MS`, `OPEN_GUARD_MS`, `PRELOAD_MAX_BYTES`), `viewer-preload.ts`
  (`ViewerPreloader`), `viewer-info.ts` (the info table).
- The info panel is built from ONE array: add a line to `INFO_ROWS` (`{ id, label, value(ctx) }`, `null`
  leaves the row out; it renders as `tr[data-info-row=id]`). `viewerInfoOpen` is its persisted open state.
  Nothing there uses the clipboard: the app is served over plain HTTP, where it does not exist.
- Each step aborts the previous detail request and asks for the new file's one `DETAIL_DEBOUNCE_MS` later
  (the heading says "Loading…", the stage dims the old picture with a spinner). A file whose row is known
  needs no detail until the panel is open. Once the current picture is shown, the next file in the
  direction of travel is preloaded: detail, large thumbnail, then `/preview` only for an image of at most
  `PRELOAD_MAX_BYTES`. A video is never preloaded beyond its poster.
- `@/api/files`: `previewUrl(id)`, `contentUrl(id, { download? })` (a plain URL for an `<img>`, `<video>` or
  link) and `fetchFileDetail(id, signal?)`. A link in the viewer needs `target` or `download`, or the router
  takes the click. `loadImage(url)` (`@/util/load-image`) loads and decodes off-screen and cancels by
  clearing `src`. `formatFileTime` (`@/util/format`) formats a mtime. `KIND_ICON` and `hasThumbnail(kind)`
  come from `@/components/apm-file-card`.
- `apm-modal size="full"` is a full-screen flex column with `p-3 sm:p-6`; its body wrapper is
  `flex min-h-0 flex-1 flex-col` with no vertical padding, so content can fill it. The other sizes are
  unchanged. The last file stays painted in the closed dialog, for its fade-out, until the next open.
- Tests: use fake timers (the opening guard swallows clicks for 400 ms), `FakeImage` (`@/test/fake-image`,
  `vi.stubGlobal('Image', FakeImage)`) and `fileDetail()` (`@/test/file-item`); mock `api.get` for
  `/api/files/{id}`. happy-dom has no layout and does not focus a dialog it opens, so check the layout,
  the focus and the Range requests in a browser.

## Filter bar
`<apm-filter-bar>` (`@/components/apm-filter-bar`, host `block`) is the filter row of every tool: cameras,
rating, flags, tags, a chip while a selection restricts the listing, "Clear filters" and the "Hide
folders without matches" switch, plus an optional "Apply to" selector. It is CONTROLLED and reads no
store: properties in, bubbling events out; the host stores the change and feeds it back.
```ts
html`<apm-filter-bar .filter=${filterFor(tool)} .contexts=${contexts} .scope=${scope}
  .hideUnmatched=${hideUnmatchedFolders.value} .selectionLabel=${label}
  @filter-change=${(e: FilterChangeEvent) => setFilter(tool, e.detail)}
  @scope-change=${(e: ScopeChangeEvent) => setScope(tool, e.detail)}
  @hide-unmatched-change=${(e: HideUnmatchedChangeEvent) => { hideUnmatchedFolders.value = e.detail; }}
  @selection-clear=${() => setFilter(tool, { ...filterFor(tool), selection_id: null })}
></apm-filter-bar>`;
```
- Properties: `filter`, `contexts` (`BrowseRequest[]`, one per pane the filter applies to; only
  `volume_id` and `dir` are read, the bar adds its own filter and no plan, and equal locations count
  once), `scope` (`'both' | 'source' | 'comparison' | null`; `null` hides "Apply to"), `scopeLabels`
  (partial `ScopeLabels` over Both/Source/Comparison), `hideUnmatched` (attribute `hide-unmatched`),
  `selectionLabel` (attribute `selection-label`; names `filter.selection_id` on the chip, fallback
  "Selection"), `disabled` and `disabledReason` (attribute `disabled-reason`; every control is off, the
  reason shows and is linked with `aria-describedby`).
- Events, all bubbling, `detail` is the VALUE itself: `filter-change` (the normalised `Filter`),
  `scope-change` (`FilterScope`), `hide-unmatched-change` (`boolean`), `selection-clear` (`null`). They
  fire on a choice of the user, never while typing in a search box, and a disabled bar emits nothing.
  "Clear filters" emits `filter-change` with the empty filter and, when a selection was set,
  `selection-clear` too. The events of the inner controls never leave the bar.
- Facets: `fetchFacets(request, { signal })` and `mergeFacets(parts)` (`@/api/facets`, `POST
  /api/facets`). The bar asks once per distinct location of `contexts`. One location gives the server's
  lists WITH counts (the rating buttons show them); several give the union by name (case-insensitive,
  first spelling wins, sorted) WITHOUT counts, because adding counts up would lie. The whole library
  takes about 0.5 s and a folder under 40 ms, so the bar NEVER waits: the controls work at once, the
  lists show "Loading…" and keep the values they had. A new filter or location asks 250 ms later and
  aborts the request in flight (a late answer is dropped); an `fs_change` at or below a location asks
  2 s after the last event of a burst (at most 10 s after the first); a stream resync and the end of an
  `index` task ask at once. A failure (one location failing fails all) keeps the old values and shows
  "Filter options unavailable" with a Retry. A chosen value the facets do not list stays visible and
  removable.
- `stores/filters.ts`: `filterFor(tool)` (`EMPTY_FILTER` until set; read it where signals are tracked),
  `setFilter(tool, filter)` (normalises; a filter that selects the same files is a no-op),
  `resetFilters(tool?)` (also drops `selection_id`, keeps the scope), `scopeFor(tool, initial = 'both')`,
  `setScope(tool, scope)` and the persisted signal `filterScopes` (`apm.v1.filterScope`). Filters are in
  memory only, so a reload clears them on purpose. `hideUnmatchedFolders` stays in `prefs`.
- `@/util/filter`: `EMPTY_FILTER` (frozen), `normalizeFilter` (trims, de-duplicates by `textKey`
  keeping the first spelling, sorts, drops ratings outside 0 to 5, puts flags in a fixed order; like
  `Filter::normalise`), `activeFilterCount` (the DIMENSIONS in use, a selection counts one),
  `isFilterActive`, `filterKey` and `sameFilter` (equal for filters that select the same files, like
  `Filter::cache_key`), `textKey` and `sameText` (the server's name comparison: NFC, lower case),
  `toggleIn`, `FLAGS`, `FILTER_SCOPES`, `DEFAULT_SCOPE_LABELS`, `MAX_RATING`.
- In a tool view: `contexts` are the `ctx.request(pane)` of the panes the filter applies to (skip the
  `null`s); `scope` is set only for `applies: 'choice'` (else `null`), `disabled` and `disabledReason`
  for `'none'`; `selection-clear` also clears the selection's hand-off entry. A hand-off sets a
  selection with `setFilter(tool, { ...EMPTY_FILTER, selection_id })`. Never wrap the bar in a container
  of its own that clips or scrolls (`overflow-hidden`, `overflow-x-auto`): the panels of the
  multi-selects are absolutely positioned dropdowns that overlay what follows the bar. The shell's
  `<main>` is fine, the panels stay inside it.
- `apm-segmented` (`@/components/apm-segmented`): `mode` (`'single'`: a radio group, one tab stop,
  arrows move and choose; `'multi'`: toggle buttons with `aria-pressed`), `options`
  (`SegmentOption { value; label; name?; icon?; title?; count?; disabled? }`: `name` is the accessible
  name when it differs from the short `label`, "3 stars" for "3"; a `count` shows as a small number and
  is added to the accessible name), `value` (a string, a `string[]` in `multi`), `label` (the group's
  accessible name),
  `disabled`. Event `segmented-change`, `detail` is the new value (a `string`, a `string[]` in
  `multi`). Controlled: nothing changes until the host feeds `value` back. The host is `inline-flex
  max-w-full` and a row that does not fit scrolls sideways below `sm` (the rating strip with counts is
  wider than a phone).
- `apm-multi-select` (`@/components/apm-multi-select`): a button that opens a searchable checklist.
  `options` (`{ value; count? }`), `selected`, `label`, `placeholder`, `icon`, `allow-custom` (an
  `Add "text"` row, and Enter adds the typed text), `loading` (a "Loading…" row; the values known stay),
  `disabled`, `max-visible` (default 100; more rows ask to type). The chosen values also show as chips
  beside the button, each with a "Remove X" button. Event `multi-select-change`, `detail` is the new
  `string[]`; typing alone never fires it. Values compare with `sameText`, so "canon eos r5" ticks
  "Canon EOS R5". It closes on Esc, on a press outside and when the focus leaves. The panel opens
  under the button (`sm:w-72`), flips to `dropdown-end` when it would stick out on the right (measured
  from the start-aligned state on every update and on window `resize` while open, because choosing a
  value can reflow the bar and move the button) and spans the window (`max-sm:fixed max-sm:inset-x-2`)
  below `sm`.
- Tests: `mount('apm-filter-bar', props)`, stub `api.post` for `/api/facets` and switch to
  `vi.useFakeTimers()` AFTER mounting; `vi.advanceTimersByTimeAsync(250)` passes the debounce. Call
  `events.stop()` in `afterEach`: the first connection after a `stop()` also runs the resync handlers.

## Pane and folder list (`apm-pane`, `apm-folder-list`, `FoldersController`, `apm-select`)
`<apm-pane>` is the file-system pane that every tool view shows TWICE (`pane="source"` and
`"comparison"`): a header (Up, breadcrumbs, heading, sort), the subfolders as a list, then the files as an
`apm-file-grid`. It reads the per-pane stores itself (`nav.locations`, `prefs.sort`, `prefs.gridSize`,
`selection`) and fetches its own data, so a tool view passes only what its tool adds. Its host is
`block h-full min-h-0 min-w-0`: give it a parent with a definite height, and keep it at least `gridSize + 16`
px wide (a listing of more than 200 files is virtualised and renders no card in a narrower pane). ONLY
`[data-pane-body]` scrolls.
```ts
html`<apm-pane pane="comparison" heading="Comparison" .filter=${filter} .plan=${plan}
  ?hide-unmatched=${hideUnmatchedFolders.value} show-matches
  @pane-counts=${(e: PaneCountsEvent) => (footers[e.detail.pane] = e.detail.counts)}
  @plan-stale=${(e: PlanStaleEvent) => rebuildPlan(e.detail)}
  @item-open=${(e: ItemOpenEvent) => openIn(e)}></apm-pane>`;
```
- Properties: `pane`, `heading` (the header badge and the name of the region), `.filter` (`Filter`;
  `EMPTY_FILTER` from `@/util/filter` by default, pass `filterFor(tool)` only to a pane the filter applies
  to), `.plan` (`PlanRef | null`: the overlay of the folders and the grid),
  `hide-unmatched`, `show-matches`, `disabled` with `disabled-reason` (inert: the heading and the reason,
  no request, no counts). The base folder row: `base-mode` (`'none'` default; `'current'`: a badge, the
  browsed folder IS the base; `'pinned'`: a chip with Use this folder, Go to base and Reset), `.base` (the
  EFFECTIVE base, `PaneLocation | null`) and `.baseDefault` (`{ label, active } | null`, `pinned` only:
  `active` says `.base` is the tool's default). The pane never decides the base: it emits `base-change`.
- Events, all bubbling: `pane-counts { pane, counts: FooterCounts | null }` (only when they change, `null`
  while loading or disabled: hand it to `PaneFooter`), `plan-stale { pane, plan }` (ONCE per plan: the
  server forgot it, so build a new one and set `.plan`; until then the rows stay and the grid shows
  placeholders), `base-change { pane, base: PaneLocation | null }` (`null` = back to the default) and
  `item-open { id, index }` (from the selection controller).
- **The pane never opens the viewer**: the tool view does, on `item-open`, with `event.target.closest(
  'apm-pane')` and its read-only `browseRequest` (`null` while the volumes are unknown or the pane is
  disabled; built by `requestFor(location, filter, plan)` in `@/api/browse`: nobody builds a
  `BrowseRequest` by hand), `sortSpec` and `itemAt(index)` (the loaded item: the viewer's `startItem`).
  `refresh()` fetches the folders, the counts and the files again (the rows stay until the answer): the
  pane already does it after an `fs_change`; `ToolContext.refresh()` (see Tool view) calls it on both.
- The pane owns: navigation (a folder row, a crumb and Up call `setLocation(pane, ...)`, one history entry
  each, the other pane untouched; each is a real `<a href>` from `linkToLocation(pane, location)` in
  `@/stores/nav`, and `isPlainClick(event)` in `@/util/plain-click` tells a click the app handles from one
  the browser keeps: middle click, modifier, new tab), the selection wiring (context key, an ids provider
  with the real files in the CURRENT sort, `reconcile()` after an `fs_change` of its folder), one scroll
  position (reset to the top when the folder, sort, order or filter changes), the walk-up from a folder that
  disappeared (`folder_not_found`: `setLocation(..., { replace: true })` to the nearest ancestor that exists
  and ONE toast per chain), the loading, error and offline-volume states, and putting the keyboard focus
  back after a keyboard-opened folder. The volumes root (`volume: null`) lists the volumes as folders:
  there is no grid and no file there, and its ids provider answers `[]`.
- Hide or grey: `folderState(entry, hideUnmatched)` (`@/util/folder-state`). A folder is UNMATCHED when
  `count === 0` or `matches === 0` (`matches` is `null` outside a plan). Unmatched is `hide` when the
  preference says so (`prefs.hideUnmatchedFolders`), else `grey` (`opacity-50`, still a link); anything
  else is `show`. A folder the plan creates (`overlay.planned`) or deletes (`overlay.will_delete`) is part
  of the plan: never hidden, greyed at most, and told in text ("new", "will be deleted"), not by colour
  alone. `emptyTextFor(...)` words the grid's empty state from what the folder list shows: a folder that
  only holds subfolders is not empty, and the filters are blamed only while they are on.
- `<apm-folder-list>` is presentational: `.folders`, `.hrefFor`, `hide-unmatched`, `show-matches`, `pane`,
  `loading` (skeletons for the first load only); event `folder-open { entry }` for a plain click. Rows use
  `content-visibility: auto` with the REAL row height (35px) as the intrinsic size, no virtualiser: a wrong
  intrinsic size makes the scroll height drift while scrolling. `crumbsFor(location)`,
  `parentLocation(location)` and `ROOT_CRUMB_LABEL` ("Volumes") are in `@/util/breadcrumbs`; segments are
  shown verbatim, never normalised.
- `FoldersController` (`@/controllers/folders-controller`, one per pane): call `setRequest(BrowseRequest |
  null)` in `willUpdate`, read `folders`, `counts`, `status` (`idle | loading | ready | error`), `error`,
  `refreshing`, `missing` (`folder_not_found`) and `planMissing` (`plan_not_found`, the data stays);
  `retry()` and `invalidate()`. A request for another listing starts over, a plan-only change refetches over
  the rows, a refetch never clears them. It refetches itself (debounced like `PagedItems`) after an
  `fs_change` below the folder, at once on a resync and when an `immich_sync` task ends.
- `<apm-select>`: the ONE drop-down of the app, a light-DOM wrapper over the DaisyUI `select`; **never write
  a raw `<select>`**. Properties: `.options` (`{ value, label, title?, disabled? }[]`), `value`, `label`
  (the accessible name: always set it), `show-label` (also shows it inside the control), `size` (`xs | sm
  | md`, default `sm`), `disabled`. Event: bubbling `select-change { value }` (the native `change` is
  stopped). It is CONTROLLED: it keeps no value, so a host that does not accept a change leaves `value`
  as it was and the control snaps back. It is as wide as its longest option (`w-auto`).
- Tests: `stubBrowseServer()` (`@/test/browse-server`) stubs `fetch` for `GET /api/volumes` and `POST
  /api/browse/{folders,items,ids}`: change `server.state` between requests (`folders`, `total`, `ids`,
  `volumes`), read `server.calls`, freeze an endpoint with `server.hold('folders')` and `release`.
  `folderEntry({ ... })` (`@/test/folder-entry`) builds a `FolderEntry`. Remove the mounted panes BEFORE
  `vi.unstubAllGlobals()` (your `afterEach` runs before the `mount` cleanup): a pane that re-renders after
  the stub is gone talks to the real network ("socket hang up"). The volumes store is a singleton that
  never goes back to `idle`, so a test of its first load needs its own file (`apm-pane.volumes.test.ts`).
  A removed pane can still have an update queued: the pane stops touching the shared stores once it is
  disconnected. Keep that.

## Tool view (`apm-tool-view`, `ToolConfig`, `ToolContext`, `src/tools/framework/`)
Every tool page is ONE declarative `ToolConfig` that `<apm-tool-view .config=${config}>` renders: a tool
writes no layout, no event wiring and no store plumbing, and the framework never knows a tool by name.
`tools/browse/` is the model (`tool.ts` holds the config, `apm-browse-page.ts` the page element). A tool
adds only its own folder `tools/<tool>/` and its route line (see Adding a page).
```ts
// tools/tidy/tool.ts
export const tidyTool = defineTool({
  id: 'cleanup', // YOUR tool's id: one of the ToolIds of @/stores/nav
  title: 'Tidy up',
  summary: 'Pick files in the source pane and move them to the trash.',
  panes: {
    source: { heading: 'Files', baseMode: 'none', showMatches: false, planSide: null },
    comparison: { heading: 'Reference', baseMode: 'none', showMatches: false, planSide: null },
  },
  filter: { applies: 'source' },
  recursive: 'toggle',
  actions: () => ({
    defaultId: 'trash',
    count: (ctx) => ctx.selection('source').count.value,
    options: [{
      id: 'trash', label: 'Move to trash', icon: 'trash-2', danger: true,
      disabled: (ctx) => (ctx.selection('source').count.value === 0 ? 'Select files first.' : null),
      run: async (ctx) => {
        const ids = ctx.selection('source').toArray();
        if (!(await confirmDialog({ title: `Trash ${ids.length} files?`, message: '...', danger: true }))) return;
        await trashFiles(ids); // the tool's own API call
        ctx.clearSelections();
        ctx.refresh();
      },
    }],
  }),
});

// tools/tidy/apm-tidy-page.ts
export class ApmTidyPage extends ApmElement {
  override connectedCallback() {
    super.connectedCallback();
    this.classList.add('block', 'h-full', 'min-h-0'); // the router does it too; a test mount does not
  }
  protected override render() {
    return html`<apm-tool-view .config=${tidyTool}></apm-tool-view>`;
  }
}
customElements.define('apm-tidy-page', ApmTidyPage);

// routes.ts: replace only your tool's comingSoon line
{ path: '/cleanup', tag: 'apm-tidy-page', label: 'Tidy up', icon: 'trash-2', menu: 'tools',
  load: () => import('@/tools/tidy/apm-tidy-page') },
```
- **`ToolConfig`** (`tool-config.ts`; `defineTool` checks it and throws `Tool "<id>": <problem>` when the
  module is imported, so a mistake fails the tool's own test): `id`, `title` (the tool bar's `h2`),
  `summary` (one sentence under it), `panes` (a `PaneSpec` per pane: `heading`, `baseMode`
  `none | current | pinned`, `showMatches`, `planSide` (`PlanSide | null`, set exactly when the tool has a
  `plan`), optional `defaultBase { from, label }`: `pinned` only, until the user pins a folder the base is
  the base of pane `from`), `filter` (`{ applies: 'both' | 'source' }`; `{ applies: 'choice', labels?,
  initial? }` shows the "Apply to" selector; `{ applies: 'none', reason }` shows the bar disabled and gives
  both panes `EMPTY_FILTER`), `recursive?` (`'always'` by default, `'toggle'` adds the "Include
  subfolders" switch, on by default), `controls?(ctx)` (the tool's own controls in the tool bar, built from
  shared components), `plan?` (`{ kind: PlanKind, params(ctx) }`, below), `actions?(ctx)` and
  `setup?(ctx)`. `controls` and `actions` run INSIDE the view's render: what they read from signals
  re-renders the view. `setup` runs once per connection (again for a new `config`) and returns its cleanup,
  `(() => void) | undefined`.
- **`ToolContext`** (`tool-context.ts`): one per connected view and the only argument a callback needs;
  its getters read signals. `config`, `tool`, `host`, `plan`; `location(pane)`; `base(pane)`,
  `setBase(pane, base)` (`pinned` panes only), `baseIsDefault(pane)`; `scope(pane)` (`Scope | null`);
  `recursive`; `toolFilter`, `filter(pane)` (the EFFECTIVE filter: `toolFilter` or `EMPTY_FILTER`),
  `filterScope` (`FilterScope`, `null` for `applies: 'none'`); `counts(pane)` (`null` while loading),
  `selection(pane)`; `request(pane)` (the pane's `BrowseRequest`, with its effective filter and the plan
  of its `planSide`); `refresh()` (both panes reload, the rows stay until the answer) and
  `clearSelections()`. `base(pane)`: `none` is `null`, `current` the location, `pinned` the folder the user
  pinned, else the base of `defaultBase.from`, else `null`. `scope(pane)` is `{ volume_id, dir, recursive }`
  of `base(pane)` (of the location for `none`) and `null` while there is nothing to scope: no base, the
  volumes root, a volume that is unknown or not loaded.
- **Plan seam** (`plan-binding.ts`): the view, the panes and the actions only ever see `PlanBinding {
  status, error, planId, stats, warnings, ref(side), stale(ref), retry(), dispose? }` with `PlanStatus` =
  `none | idle | building | ready | executing | error`. `createPlanBinding(ctx)` returns the frozen
  `NO_PLAN` until the plan client lands: the client replaces ONLY that body (`NO_PLAN` for a tool without
  `ctx.config.plan`), keeps its state in signals, and extends `renderPlanStatus` and `actionReason` when it
  needs more. `ctx.plan` is built last in the constructor, so the binding can read the whole context. The
  view gives each pane `plan.ref(planSide)`, forwards `plan-stale` to `plan.stale(ref)` and calls `dispose`
  when it disconnects or gets another `config`. `plan.params(ctx)` is `{ ready: true, params }` or `{ ready:
  false, reason }` (the reason is quiet text in the tool bar, `null` shows nothing).
- `renderPlanStatus(ctx)` (`plan-status.ts`): a persistent `[role=status]` ("Matching..." and a spinner
  while `building`, "Working..." while `executing`, else the `params` reason), an inline `alert-error` with
  Retry (`plan.retry()`) on `error`, and an "N warnings" dropdown listing `plan.warnings`.
- **Actions** (`tool-actions.ts`): `ToolActions { options, defaultId, count? }`, `ToolAction { id, label,
  icon?, danger?, hint?, disabled?(ctx), run(ctx) }`. One option is a `btn-lg` primary button (red when
  `danger`), several the `apm-split-button`, which remembers the choice per tool (`lastAction`). `disabled`
  returns the reason (also the tooltip), `null` when the action can run. `run` owns its confirmation
  (`confirmDialog`); a rejection becomes ONE `toast.error` except an abort, which is silent; a click while
  busy is ignored. `actionReason(ctx, action)` is the one place that decides whether an action can run: the
  plan client extends it ("no plan yet"). `checkToolActions` (duplicate ids, unknown `defaultId`) runs at
  render and logs once per distinct problem; nothing is rendered then.
- **`followSource(ctx, { enabled? })`** (`follow-source.ts`), returned from `setup` (Folder formatting,
  Empty folder cleanup): the comparison pane starts on, then follows, the source folder, e.g. `setup: (ctx)
  => followSource(ctx, { enabled: (c) => c.baseIsDefault('comparison') })`. It writes with `replace: true`
  and NEVER adds a history entry, so Back steps through the user's own moves. The first move is immediate;
  a later one lands one microtask AFTER the move of the source (read the comparison after an `await`) and
  moves of one turn are followed once. `enabled` is asked on every move, not subscribed to. Nothing moves
  while the source has no volume.
- **`stores/tool-state.ts`**, in memory per tool (a reload clears it on purpose): `pinnedBase` and
  `setPinnedBase(tool, pane, base)`, `recursiveFor` and `setRecursive(tool, on)`, `handoffFor` and
  `setHandoff(tool, { label } | null)`, `resetToolState(tool)`, `resetToolStateForTests()`; in a tool use
  the `ToolContext` getters. Hand-off (another tool sends the user here with a server-side selection):
  `setFilter(tool, { ...EMPTY_FILTER, selection_id })` plus `setHandoff(tool, { label })`; the label is the
  filter bar's chip, and the bar's `selection-clear` clears both (the view does it).
- **What the view wires**, so a tool never does: the filter bar (`filter-change`, `scope-change` (the
  selector shows only for `applies: 'choice'`), `selection-clear`, `hide-unmatched-change`); the panes
  (`pane-counts`, `plan-stale`, `base-change` into `ctx.setBase`, and `item-open`, which opens the viewer on
  THAT pane's listing through `event.target.closest('apm-pane').itemAt(index)` and `openViewer`); the bottom
  bar (`size-change`, `select-all`, `clear-selection`). It also installs the selection shortcuts and starts
  with both selections empty. The render is `keyed` by the context: another tool or a reconnection gets
  new panes and a new bar.
- **Layout** (checked in a browser from 320 to 1400 px): the host is `flex h-full min-h-0 min-w-0 flex-col`,
  so the page element is `block h-full min-h-0` and NOTHING but a pane body scrolls: never add a scrolling
  wrapper. Children, top to bottom: the tool bar, the filter bar, the panes and the bottom bar. The panes
  sit side by side from `md` (a pane is then at least 383 px wide, more than the largest card plus its
  padding) and stacked in two rows below it. The tool bar has two rows: the title, the switch and
  `controls` (wrapping), then the summary with the plan status at the right end of the SAME row. That row
  keeps its height, so no plan status moves the panes (only the error alert makes it taller): keep
  `controls` free of content that changes height. A tool without a plan has a 48 px bar.
- **Grid and pane additions**: `ApmPane.refresh()` and `ApmFileGrid.refresh()` reload the lists (what is
  shown stays until the answer arrives); `ctx.refresh()` asks both panes. `apm-file-grid` limits the card
  size to its own width, because the virtualiser renders no card in a grid narrower than one card (a phone
  with the largest size).
- Tests: `toolConfig(patch)`, `plainPane(heading)` and `FakePlan` (`@/test/tool-config`). Fake the plan
  seam by mocking `createPlanBinding` of `@/tools/framework/plan-binding` (see the top of
  `apm-tool-view.test.ts`); mock `@/components/apm-viewer` when the viewer is not under test; use
  `stubBrowseServer()` for the panes. Reset in `afterEach`: `resetToolStateForTests()`, `resetFilters()`,
  `resetSelectionForTests()`, and remove the views BEFORE `vi.unstubAllGlobals()`.

## Index controls (`api/indexing.ts`, `stores/index-status.ts`, `app/reindex.ts`, `apm-index-notices`)
The server's `IndexJobs` is the only thing that starts index work (scan, metadata, checksums,
thumbnails). The UI asks for it through ONE endpoint and watches the `index` task in the task indicator,
so **nothing here has a progress UI of its own**. Identical requests coalesce into the job that is already
queued or running, so asking twice is harmless.
- `@/api/indexing`: `rescan(request: RescanRequest, signal?)` is `POST /api/index/rescan` (202, the
  `TaskDto`) and `getIndexStatus(signal?)` is `GET /api/index/status` (`IndexStatusDto`: `held_removals`,
  `active_task_id`). `RescanRequest` is `{ targets: Scope[], force, confirm_removals }`: `targets: []` means
  every volume, a target with `volume_id: null` and `dir: ''` every online volume, `force: true` treats
  every file in scope as changed (metadata, checksums and thumbnails are redone), `confirm_removals: true`
  lets a scan of that scope drop the entries it would otherwise hold back. Errors: 400 `invalid_path`,
  404 `not_found`, 409 `volume_offline`.
- `stores/index-status.ts`: `indexStatus` (`ReadonlySignal<IndexStatusDto>`, nothing held until the first
  answer), `refreshIndexStatus()` (never rejects: a failure or an answer that is not an `IndexStatusDto`
  is ignored, concurrent calls coalesce, an unchanged answer keeps its identity), `startIndexStatus()`
  (the shell calls it once: loads now, again after a resync and whenever an `index` task ends) and
  `resetIndexStatusForTests()`. Nothing else needs to refresh it: a scan you start ends with an `index`
  task event.
- `app/reindex.ts`: `reindexCurrentFolders()` (recursive and forced, on the folders of BOTH panes,
  `currentFolderTargets()`: the same folder in both panes is sent once, a pane at the volumes root shows
  none and the info toast says "Open a folder first"), `reindexAllAssets()` (a `confirmDialog` first,
  then `targets: []`), `reindexPending` (`ReadonlySignal<ReindexScope | null>`: the request being sent,
  a confirmation not answered yet included), `reindexDisabled` (true while one is being sent and while
  `events.connection` is `reconnecting` or `closed`: a task started then could not be followed) and
  `startReindex()` (registers the `onReindex` handler; the shell calls it once). An offline volume is not
  filtered out here: the server answers 409 `volume_offline` and its message becomes the error toast.
- `<apm-index-notices>` (no properties, no events; host `block flex-none`; the shell renders it between
  the connection banner and `<main>`): when a scan would remove at least 50 entries and more than a
  fifth of what a volume had (a share that is not mounted looks exactly like that), the scanner holds ALL
  removals of that volume back and reports the count in `held_removals`. One `alert alert-warning` per
  volume says how many files were not found, with "Rescan" (`force: false`: the entries are fine again
  when the share is back) and "Remove from the index" (a danger `confirmDialog` with the count and the
  volume name, then `confirm_removals: true`; no file on disk is ever touched). While a request runs
  every button of the notices is disabled and the pressed one shows a spinner. A notice goes away by
  itself when the next scan ends.
- Tests: `shownConfirm()` and `pressConfirm(label)` (`@/test/confirm`) drive the shared confirm dialog
  (the module also removes stray dialogs after each test). Set the connection with
  `(events.connection as Signal<ConnectionState>).value = 'open'`. In `apm-app.test.ts` every test but the
  first mounts with the connection `reconnecting` (the shared client resyncs after `events.stop()`), so
  call `FakeEventSource.last().open()` before expecting an enabled Re-index entry or a notice without the
  connection banner above it. Call `resetIndexStatusForTests()` in `afterEach`.

## Trash page (`tools/trash/`, `api/trash.ts`, `test/trash-fixtures.ts`)
The `/trash` route (`apm-trash-page`, footer menu, icon `trash-2`) lists what is in the trash, restores
it and empties it. The server side is the `trash` service (root AGENTS.md, "Trash"); the page only
calls it and shows what it answers.
- `@/api/trash`: `listTrash(query, signal?)` (`GET /api/trash`, `TrashListQuery`: send every field,
  `null` for none; `order: null` is the natural direction of the sort), `listTrashBatches(query, signal?)`
  (`GET /api/trash/batches`, the deletions), `restoreTrash(request, signal?)`, `emptyTrash(request,
  signal?)` and `syncTrash(signal?)` (the POSTs answer the `TaskDto`). Both lists embed the GLOBAL
  `summary` (items, bytes, `max_id`, `retention_days`, `conflict_policy`, and `volumes` that hold
  trash with `online`/`writable`); the page takes its totals from whichever answer came last.
- Files: `apm-trash-page.ts` is the only one that fetches, keeps state and runs actions.
  `apm-trash-deletions.ts` and `apm-trash-items.ts` are presentational (properties down, bubbling
  events up: `trash-restore-batch`, `trash-view-batch`; `trash-search`, `trash-volume`, `trash-sort`,
  `trash-select`, `trash-select-page`, `trash-restore-items`, `trash-page`, `trash-clear-batch`,
  `trash-clear-filters`). `trash-parts.ts` holds what both tabs share (volume `apm-select`, volume
  badges, pager, skeleton, empty state). `trash-text.ts` holds EVERY sentence the page says
  (retention rule, confirmations, why a button is disabled, task outcomes): the end-to-end tests key
  off that wording, so change it only together with them.
- Layout: heading "Trash", the totals (an em dash until the first answer), one badge per volume that
  holds trash ("photos · 10 KB", plus "offline" or "read-only"), "Empty trash…" (a Unicode ellipsis;
  "Empty trash on NAME…" while one volume is filtered), a "More actions" menu with "Look for
  untracked items", the retention note with a "Change in Settings" link, then the tabs "Deletions"
  and "Items" (`role=tablist`, arrow keys, Home and End; only the open panel is in the DOM, and the
  Items tab loads the first time it opens).
- Deletions: one row per deletion, newest first (age, volume, items, size, reason badges), "Restore
  all" (a confirmation with the numbers, the volume and what the `conflict_policy` of the summary
  does; the request is `{ target: { kind: 'batch', volume_id, batch }, conflict_policy: null }`) and
  "View items", which opens the Items tab on that deletion with a chip, dismissed by "Show every
  deletion" (the volume stays). Items: search ("Search by path", 300 ms debounce, trimmed), the
  volume select (shared with Deletions; a volume that no longer holds trash falls back to all of
  them), sortable columns "Name", "Original folder", "Size" and "Deleted" (a column starts in its
  natural direction, a second click flips it), a checkbox per row and one for the page, "Restore
  selected (N)" (no confirmation, `{ target: { kind: 'ids', ids } }`), a "Restore" per row and the
  pager (`PAGE_SIZE` = 50). The selection is cleared by a page change, a sort, a search or a filter,
  and a reload drops the ids that are no longer on the page. Rows of an offline or read-only volume
  cannot be selected or restored; the `title` of the disabled control says why (`title` is the
  tooltip here, as elsewhere in the app).
- "Empty trash…" asks with the numbers on screen and sends `{ volume_id, up_to_id: summary.max_id }`:
  both are taken BEFORE the dialog opens, so what is deleted is what the user was shown even when the
  summary changes meanwhile. It is disabled, with the reason as a `title` on the button and on its
  wrapper, while loading, when nothing is deletable, and for an offline or read-only volume; with
  several volumes the question names those that stay behind. `409 trash_already_empty` is an info
  toast plus a reload; any other failure is an error toast with the server's message.
- Tasks: the page toasts the start of what it asked for ("Restoring 3 items from the trash"), then
  the outcome from the task's title and `error` (`taskOutcome`): success, a warning when some items
  stayed in the trash or a check kept or skipped something, an error for `failed` and `interrupted`,
  an info for `cancelled`. It shows no progress of its own: the task indicator does. An ending that
  arrives before the POST has answered is remembered, so its outcome still toasts once.
- Reloading: the open tab reloads (the other one when it opens) 500 ms after an ended task of kind
  `fs_move`, `fs_trash`, `plan_execute` or `trash_empty` (never `index`: a scan cannot change the
  trash; bursts coalesce), after the event stream resyncs and when the window regains focus. A new
  request supersedes the one in flight (its answer is ignored), a reload keeps the rows on screen, a
  page that no longer exists steps back to the last one and a failed load is an `alert` with "Retry".
- Tests: `apm-trash-page.test.ts` fakes the server with `vi.spyOn(api, 'get' | 'post')`, runs on
  fake timers with the clock three days after `TRASHED_AT` (so ages read "3 days ago") and drives the
  volume `<select>` by setting its `value` and dispatching `change`. `@/test/trash-fixtures` has
  `makeTrashItem`, `makeTrashBatch`, `makeTrashVolume`, `makeTrashSummary` (items and bytes are the
  sums over its volumes) and `TRASHED_AT`. Toasts expire after a few seconds of fake time and
  identical ones do not stack: spy on `toast.success` and friends when a test advances the clock.

## Plans
All plan tools share `createPlanBinding(ctx)` (`@/tools/framework/plan-binding`), backed by
`PlanController`. No tool implements its own build/debounce/execute/result loop or progress UI.
The shell's task indicator continues showing progress, speed/ETA and cancel while users browse
or leave the tool. Only explicit confirmation and Details open a modal.
- `PlanSpec.params(ctx)` is pure and reads signals; return `{ ready: false, reason }` until bases
  and controls are valid, otherwise `{ ready: true, params }` with the exact planner JSON shape.
  Embed generated `Scope` objects (from `ctx.scope(pane)`) in those parameters so filesystem
  invalidations can find both sides. Keys are sorted recursively by exported `paramsKey`;
  arrays and exact on-disk path bytes are preserved.
- Parameter changes disable actions immediately and build after 300 ms, aborting obsolete
  requests. Old preview refs remain while rebuilding (no pane flash). Filesystem changes and
  stream resync coalesce with 1.5 s quiet / 5 s maximum wait. Terminal index tasks rebuild only
  when a shown warning has an `INDEX_DEPENDENT_WARNINGS` code (`unhashed_files`,
  `metadata_pending`). `ctx.plan.retry()` rebuilds now, except while a request or execution is
  in flight; `stale(ref)` rebuilds expired previews with a loop guard.
- Declare actions through the existing `ToolAction.run`; e.g.
  ```ts
  run: (ctx) => executePlanAction(ctx, {
    side: 'comparison', options: { action: 'delete_comparison' },
    confirm: ({ count, bytes, selected }) => deleteConfirmation(count, bytes, selected),
    emptyMessage: 'No matched files to delete.',
  }),
  ```
  `executePlanAction` (`@/tools/framework/plan-actions`) owns the effective-selection rule:
  explicit ids from the pane mapped to `side`, restricted by the server, else all matches.
  It obtains exact counts/bytes before confirmation. `side: null` skips selection (cleanup);
  its confirmation gets null counts/bytes and can use the planner's displayed statistics.
  `confirm: null` skips confirmation. All actions of plan tools are gated on status `ready`
  before their own `disabled(ctx)` reason. Execution clears both selections only after a
  successful queue response and calls the additive `PlanBinding.executing?(taskId)` hook.
  Executing plans have no overlay; parameters are remembered and rebuilt once, 1 s after
  the task ends. A changed/expired plan yields a warning and retry, other failures reject
  to the framework's error toast.
- `handOffSelection({ target, origin, selection, label? })`
  (`@/tools/framework/hand-off`) takes an existing generated `SelectionDto`, replaces the target
  filter with `selection_id`, writes the removable chip label and source location, pins the
  source if needed, and navigates while retaining the pane URL. `target: 'move'` is the common
  duplicate-finder hand-off; pass a target config for any other tool to honor its source base
  mode without guessing. The label defaults to `selection.label`, else a grouped file count.
  The duplicate tool chooses the origin/selection (either explicit pane, else comparison);
  this helper never performs network calls.
- `@/api/plans`: `createPlan(kind, params, { replaces?, signal?, client? })`, `getPlan(id, options?)`,
  `deletePlan(id, options?)`, `executePlan(id, { options, only }, fetchOptions?)`,
  `createPlanSelection(planId, side, { restrictIds?, signal?, client? })`.
  `@/api/selections`: `createSelection(ids, label?, options?)`, `getSelection(id, options?)`.
  Every options object accepts `{ signal?, client? }`; wrappers never swallow API errors.
  DTOs: `CreatePlanRequest`, `PlanDto`, `PlanKind`, `PlanSide`, `PlanRef`, `ExecutePlanRequest`,
  `PlanSelectionRequest`, `CreateSelectionRequest`, `SelectionDto`, `TaskDto`, `PlanResult`,
  `ItemFailure` (all generated).
- `trackPlanRun({ tool, planId, taskId, title })`, `lastRun(tool)` (read-only signal),
  `dismissLastRun(tool)` and `resetPlanRunsForTests()` (`@/stores/plan-runs`) retain in-memory
  outcomes across view disposal. Every terminal state retrieves its result with one retry
  (partial results and cancellation/restart notes included); reconnects
  reconcile missed terminal tasks. A new execution replaces that tool's last result.
  Success without omissions is a success toast; failures/skips offer a warning toast's
  **Details** action and a dismissible Last run link in the status area. Other terminal states
  retain the task's title/error and use error/info/warning toasts.
- `openPlanResult(run)` (`@/components/apm-plan-result`) reuses one large `apm-modal`, DaisyUI
  stats and a bounded table of outcome details. `PlanResult.failures` can contain coded skips
  as well as failures: **never infer failed counts from rows**. Display the server's separate
  done/skipped/failed counters, bytes, cancellation/note and the first N of failed + skipped.
  Reason groups count only displayed details. `describeFailure(code, message)`
  (`@/util/failure-codes`) is the single human-reason map, with an unknown-code fallback.
- Toasts accept `{ action: { label, run }, timeout? }`; an actionable toast stays at least
  10 s (`timeout: 0` remains persistent). Repeated messages update the action rather than
  keeping an obsolete Details target.
