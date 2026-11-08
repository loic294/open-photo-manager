import { effect } from '@preact/signals-core';
import { html, nothing } from 'lit';
import { classMap } from 'lit/directives/class-map.js';
import { live } from 'lit/directives/live.js';
import { createRef, ref } from 'lit/directives/ref.js';
import { events } from '@/api/sse';
import { type ReindexScope, requestReindex } from '@/app/app-actions';
import { reindexDisabled, reindexPending, startReindex } from '@/app/reindex';
import { type AppRouter, createAppRouter } from '@/app/router';
import { type AppRoute, routes as appRoutes, notFound, redirects, routeAt } from '@/app/routes';
import { startTheme } from '@/app/theme';
import { ApmElement } from '@/base/apm-element';
import { startImmich } from '@/stores/immich';
import { startIndexStatus } from '@/stores/index-status';
import { currentPath, linkTo, startNavSync } from '@/stores/nav';
import { startTasks } from '@/stores/tasks';
import { startVolumes } from '@/stores/volumes';
// Components the shell renders. Add one import line here for a shell-level component that another
// feature provides (for example `import '@/components/apm-task-indicator';`).
import '@/app/apm-theme-toggle';
import '@/components/apm-icon';
import '@/components/apm-index-notices';
import '@/components/apm-task-indicator';
import '@/components/apm-toast-host';

const APP_NAME = 'Advanced Photo Manager';
const TOGGLE_ID = 'apm-menu-toggle';
const MENU_ID = 'apm-menu';

/** DaisyUI reveals the drawer with a short delay, and a hidden element cannot take focus. */
const FOCUS_DELAY_MS = 150;

const REINDEX_ITEMS: readonly { scope: ReindexScope; label: string }[] = [
  { scope: 'current', label: 'Re-index current folders' },
  { scope: 'all', label: 'Re-index all assets' },
];

/**
 * The application shell, the one element in `index.html`: a viewport-fixed layout that never
 * scrolls the page itself.
 *
 *   drawer (menu)  |  top bar: page heading ......... task indicator, theme toggle
 *   tools          |  connection banner (only while the server connection is down)
 *   -------------- |  index notices (only while the scanner holds removals back)
 *   re-index       |  <main>: the router outlet, one page element per route
 *   trash/settings |
 *
 * It owns the app-wide services: the theme, the nav/URL sync, the volumes, tasks and index-status
 * stores, the re-index menu actions, the SSE connection, the router and the single toast host. The
 * menu is built from the route table in `app/routes.ts`; below the `lg` breakpoint it collapses
 * into a drawer opened by the menu button.
 */
export class ApmApp extends ApmElement {
  static override properties = {
    routes: { attribute: false },
    menuOpen: { state: true },
  };

  /** The pages of the app. Defaults to the table in `app/routes.ts`; tests pass stub loaders. */
  declare routes: readonly AppRoute[];
  declare menuOpen: boolean;

  private router: AppRouter | undefined;
  private stopServices: (() => void)[] = [];
  private focusTimer: number | undefined;
  private menuButton = createRef<HTMLButtonElement>();

  constructor() {
    super();
    this.routes = appRoutes;
    this.menuOpen = false;
  }

  override connectedCallback() {
    super.connectedCallback();
    this.classList.add('block');
    this.stopServices = [
      startTheme(),
      startNavSync(),
      startTasks(),
      startVolumes(),
      startIndexStatus(),
      startImmich(),
      startReindex(),
      effect(() => {
        document.title = `${this.pageFor(currentPath.value).label} · ${APP_NAME}`;
      }),
    ];
    events.start();
    this.router = createAppRouter(this, { routes: this.routes });
  }

  override disconnectedCallback() {
    // Lit detaches the router's window listeners here; `dispose()` then removes the controller.
    super.disconnectedCallback();
    this.router?.dispose();
    this.router = undefined;
    events.stop();
    for (const stop of this.stopServices.splice(0)) stop();
    window.clearTimeout(this.focusTimer);
  }

  /** The route that explains `path`: an exact match, else where it redirects to, else not found. */
  private pageFor(path: string): AppRoute {
    return routeAt(path, this.routes) ?? routeAt(redirects[path] ?? '', this.routes) ?? notFound;
  }

  private toggleMenu() {
    if (this.menuOpen) {
      this.closeMenu();
      return;
    }
    this.menuOpen = true;
    window.clearTimeout(this.focusTimer);
    this.focusTimer = window.setTimeout(() => {
      if (this.menuOpen) this.focusMenu();
    }, FOCUS_DELAY_MS);
  }

  /** Moves the focus into the opened drawer: onto the current page's entry, else the first link. */
  private focusMenu() {
    const menu = this.querySelector(`#${MENU_ID}`);
    const target =
      menu?.querySelector<HTMLElement>('a[aria-current="page"]') ??
      menu?.querySelector<HTMLElement>('a');
    target?.focus();
  }

  /** Closes the drawer and puts the focus back on the button that opened it. */
  private closeMenu() {
    if (!this.menuOpen) return;
    this.menuOpen = false;
    this.menuButton.value?.focus();
  }

  /** The checkbox behind the drawer changes when the user clicks the overlay. */
  private onToggleChange(event: Event) {
    const checked = (event.target as HTMLInputElement).checked;
    if (checked) this.menuOpen = true;
    else this.closeMenu();
  }

  private onKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape' && !event.defaultPrevented) this.closeMenu();
  }

  /** Choosing an entry closes the drawer (it only exists below the `lg` breakpoint). */
  private onMenuClick(event: Event) {
    if (event.target instanceof Element && event.target.closest('a[href], button')) {
      this.closeMenu();
    }
  }

  private renderLink(route: AppRoute) {
    const active = currentPath.value === route.path;
    return html`
      <li>
        <a
          href=${linkTo(route.path)}
          class=${classMap({ 'menu-active': active })}
          aria-current=${active ? 'page' : nothing}
        >
          <apm-icon name=${route.icon} size="sm"></apm-icon>
          ${route.label}
          ${route.attention?.() ? html`<span class="status status-warning" role="img" aria-label="Needs attention"></span>` : nothing}
        </a>
      </li>
    `;
  }

  private renderReindex(item: { scope: ReindexScope; label: string }) {
    const disabled = reindexDisabled.value;
    return html`
      <li class=${classMap({ 'menu-disabled': disabled })}>
        <button type="button" ?disabled=${disabled} @click=${() => requestReindex(item.scope)}>
          ${
            reindexPending.value === item.scope
              ? html`<span class="loading loading-spinner loading-xs"></span>`
              : html`<apm-icon name="refresh-cw" size="sm"></apm-icon>`
          }
          ${item.label}
        </button>
      </li>
    `;
  }

  private renderConnectionBanner() {
    const state = events.connection.value;
    if (state !== 'reconnecting' && state !== 'closed') return nothing;
    return html`
      <div role="alert" class="alert alert-warning flex-none rounded-none">
        <apm-icon name="wifi-off"></apm-icon>
        <span>
          ${
            state === 'closed'
              ? 'Not connected to the server.'
              : 'Connection to the server lost. Reconnecting…'
          }
        </span>
      </div>
    `;
  }

  protected override render() {
    const open = this.menuOpen;
    return html`
      <div
        class="drawer lg:drawer-open h-screen w-screen grid-rows-1 overflow-hidden"
        @keydown=${this.onKeydown}
      >
        <input
          id=${TOGGLE_ID}
          type="checkbox"
          class="drawer-toggle"
          tabindex="-1"
          aria-hidden="true"
          .checked=${live(open)}
          @change=${this.onToggleChange}
        />
        <div class="drawer-content flex min-h-0 min-w-0 flex-col">
          <header class="navbar min-h-12 flex-none gap-2 border-b border-base-300 bg-base-100 px-2">
            <div class="flex min-w-0 flex-1 items-center gap-2">
              <button
                type="button"
                class="btn btn-ghost btn-sm btn-square lg:hidden"
                aria-label="Menu"
                aria-controls=${MENU_ID}
                aria-expanded=${open ? 'true' : 'false'}
                ${ref(this.menuButton)}
                @click=${this.toggleMenu}
              >
                <apm-icon name="menu"></apm-icon>
              </button>
              <h1 class="truncate text-lg font-semibold">
                ${this.pageFor(currentPath.value).label}
              </h1>
            </div>
            <div class="flex flex-none items-center gap-2">
              <apm-task-indicator></apm-task-indicator>
              <apm-theme-toggle></apm-theme-toggle>
            </div>
          </header>
          ${this.renderConnectionBanner()}
          <apm-index-notices></apm-index-notices>
          <main class="min-h-0 flex-1 overflow-hidden">${this.router?.outlet()}</main>
        </div>
        <div class="drawer-side">
          <label for=${TOGGLE_ID} class="drawer-overlay" aria-label="Close menu"></label>
          <aside
            id=${MENU_ID}
            class="flex h-full w-64 flex-col border-r border-base-300 bg-base-200"
            @click=${this.onMenuClick}
          >
            <div class="flex flex-none items-center gap-2 px-4 py-3">
              <apm-icon name="images" size="lg" class="text-primary"></apm-icon>
              <span class="truncate text-sm font-semibold">${APP_NAME}</span>
            </div>
            <nav aria-label="Main" class="flex min-h-0 flex-1 flex-col">
              <ul class="menu min-h-0 w-full flex-1 flex-nowrap overflow-y-auto">
                ${this.routes.filter((route) => route.menu === 'tools').map((r) => this.renderLink(r))}
              </ul>
              <div class="divider my-0 flex-none"></div>
              <ul class="menu w-full flex-none">
                ${REINDEX_ITEMS.map((item) => this.renderReindex(item))}
                ${this.routes.filter((route) => route.menu === 'footer').map((r) => this.renderLink(r))}
              </ul>
            </nav>
          </aside>
        </div>
      </div>
      <apm-toast-host></apm-toast-host>
    `;
  }
}

customElements.define('apm-app', ApmApp);

declare global {
  interface HTMLElementTagNameMap {
    'apm-app': ApmApp;
  }
}
