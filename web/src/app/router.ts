import { type BaseRouteConfig, type RouteConfig, Router } from '@lit-labs/router';
import { html, nothing, type ReactiveElement } from 'lit';
import { keyed } from 'lit/directives/keyed.js';
import { ref } from 'lit/directives/ref.js';
import { html as staticHtml, unsafeStatic } from 'lit/static-html.js';
import {
  type AppRoute,
  notFound as defaultNotFound,
  redirects as defaultRedirects,
  routes as defaultRoutes,
} from '@/app/routes';
import '@/components/apm-icon';
import { navigate, syncFromLocation } from '@/stores/nav';

const TAG_NAME = /^[a-z][a-z0-9]*(?:-[a-z0-9]+)+$/;

/**
 * `Routes` registers itself with its host BEFORE it stores its routes, so on a host that is
 * already connected it navigates with an empty table. This waits for the constructor to finish and
 * then starts exactly as Lit would have, so the router can be created at any point of the host's
 * life.
 */
class LateStartRouter extends Router {
  private ready = false;

  constructor(host: ReactiveElement, routes: RouteConfig[], fallback: BaseRouteConfig) {
    super(host, routes, { fallback });
    this.ready = true;
    if (host.renderRoot !== undefined && host.isConnected) this.hostConnected();
  }

  override hostConnected() {
    if (this.ready) super.hostConnected();
  }
}

export interface AppRouterOptions {
  /** Defaults to the table in `app/routes.ts`. Tests pass stub loaders here. */
  routes?: readonly AppRoute[];
  notFound?: AppRoute;
  redirects?: Readonly<Record<string, string>>;
}

export interface AppRouter {
  /** The current page, to render inside `<main>`. Empty until the first page has loaded. */
  outlet(): unknown;
  /** Detaches the router from its host (the host's `disconnectedCallback` calls this). */
  dispose(): void;
}

/**
 * History-mode router over `@lit-labs/router` for the route table in `app/routes.ts`.
 *
 * Create it in the host's `connectedCallback` and render `outlet()` from the host's `render()`.
 * A route's module is imported with `route.load()` the first time it is visited (once; a failed
 * import is tried again on the next visit), then `<route.tag class="block h-full min-h-0">` is
 * rendered with `route.props` assigned before it connects. A page that fails to load shows an
 * alert with a Retry button.
 *
 * Plain `<a href>` clicks and Back/Forward are handled, and every navigation re-reads the path and
 * the pane query (`syncFromLocation`) into the nav store. Links to server resources must carry
 * `download`, `target` or `rel="external"`, or the router takes them for pages.
 */
export function createAppRouter(host: ReactiveElement, options: AppRouterOptions = {}): AppRouter {
  const table = options.routes ?? defaultRoutes;
  const missing = options.notFound ?? defaultNotFound;
  const redirects = options.redirects ?? defaultRedirects;

  for (const route of [...table, missing]) {
    if (!TAG_NAME.test(route.tag)) {
      throw new Error(`Route ${route.path}: "${route.tag}" is not a custom element name`);
    }
  }

  const loads = new Map<AppRoute, Promise<void>>();
  const applyProps = new Map<AppRoute, (element?: Element) => void>();
  let navigation = 0;
  let failed: AppRoute | undefined;
  let retrying = false;

  function load(route: AppRoute): Promise<void> {
    let loading = loads.get(route);
    if (!loading) {
      loading = (async () => {
        await route.load();
        if (!customElements.get(route.tag)) {
          throw new Error(`Loading ${route.path} did not define <${route.tag}>`);
        }
      })();
      loads.set(route, loading);
      loading.catch(() => loads.delete(route));
    }
    return loading;
  }

  // The last navigation wins: a page that finishes loading after the user has moved on is dropped.
  function enter(route: AppRoute) {
    return async (): Promise<boolean> => {
      navigation += 1;
      const mine = navigation;
      syncFromLocation();
      try {
        await load(route);
        if (mine !== navigation) return false;
        failed = undefined;
      } catch (error) {
        console.error(`[router] cannot load ${route.path}`, error);
        if (mine !== navigation) return false;
        failed = route;
      }
      return true;
    };
  }

  function propsApplier(route: AppRoute) {
    let apply = applyProps.get(route);
    if (!apply) {
      apply = (element) => {
        if (element && route.props) Object.assign(element, route.props);
      };
      applyProps.set(route, apply);
    }
    return apply;
  }

  async function retry() {
    retrying = true;
    host.requestUpdate();
    try {
      await router.goto(window.location.pathname);
    } finally {
      retrying = false;
      host.requestUpdate();
    }
  }

  function failureView(route: AppRoute) {
    return html`
      <div class="flex h-full items-center justify-center p-6">
        <div role="alert" class="alert alert-error alert-soft max-w-xl">
          <apm-icon name="circle-alert"></apm-icon>
          <div>
            <h2 class="font-semibold">${route.label} could not be loaded</h2>
            <p class="text-sm">
              Check the connection to the server. If the app was just updated, reload this page.
            </p>
          </div>
          <button type="button" class="btn btn-sm" ?disabled=${retrying} @click=${retry}>
            ${retrying ? html`<span class="loading loading-spinner loading-xs"></span>` : nothing}
            Retry
          </button>
        </div>
      </div>
    `;
  }

  // Keyed by path: two routes that share a tag (placeholders) must not share an element.
  function view(route: AppRoute) {
    const tag = unsafeStatic(route.tag);
    return keyed(
      route.path,
      staticHtml`<${tag} class="block h-full min-h-0" ${ref(propsApplier(route))}></${tag}>`,
    );
  }

  function render(route: AppRoute) {
    return failed === route ? failureView(route) : view(route);
  }

  const router = new LateStartRouter(
    host,
    [
      ...Object.entries(redirects).map(([from, to]) => ({
        pattern: new URLPattern({ pathname: from }),
        enter: () => {
          navigate(`${to}${window.location.search}${window.location.hash}`, { replace: true });
          return false;
        },
      })),
      ...table.map((route) => ({
        pattern: new URLPattern({ pathname: `${route.path}{/}?` }),
        enter: enter(route),
        render: () => render(route),
      })),
    ],
    { enter: enter(missing), render: () => render(missing) },
  );

  return {
    outlet: () => router.outlet(),
    dispose: () => host.removeController(router),
  };
}
