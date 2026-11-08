import { html, nothing } from 'lit';
import { repeat } from 'lit/directives/repeat.js';
import { html as staticHtml, unsafeStatic } from 'lit/static-html.js';
import { ApmElement } from '@/base/apm-element';
import '@/components/apm-icon';
import { type SettingsSection, settingsSections } from '@/tools/settings/sections';

/** The query key that names the open section: `/settings?section=general`. */
export const SECTION_QUERY_KEY = 'section';

const TAG_NAME = /^[a-z][a-z0-9]*(?:-[a-z0-9]+)+$/;

const tabId = (section: SettingsSection) => `settings-tab-${section.id}`;
const panelId = (section: SettingsSection) => `settings-panel-${section.id}`;

/** Changes only `section`: the panes' `src` and `cmp` stay, and no history entry is added. */
function writeSection(id: string): void {
  const url = new URL(window.location.href);
  url.searchParams.set(SECTION_QUERY_KEY, id);
  const target = `${url.pathname}${url.search.replaceAll('%2F', '/')}${url.hash}`;
  window.history.replaceState(window.history.state, '', target);
}

/**
 * The Settings route: a title row, one tab per entry of `settingsSections` (see `sections.ts`)
 * and the open section below. The open section is kept in `?section=<id>`; an unknown or missing
 * id shows the first section.
 *
 * A section's module is imported the first time its tab is opened. From then on the section
 * stays in the page, hidden while another tab is open, so edits that are not saved yet survive a
 * switch of tabs. The page owns no section data: each section loads and saves its own.
 *
 * The shell owns the page's `<h1>`, so the heading here is an `<h2>`.
 */
export class ApmSettingsPage extends ApmElement {
  static override properties = {
    sections: { attribute: false },
    requested: { state: true },
    loaded: { state: true },
    failed: { state: true },
  };

  /** The tabs in order: the registry in `sections.ts`. Tests pass their own. */
  declare sections: readonly SettingsSection[];
  /** The id in the address bar; it may name no section. */
  declare requested: string | null;
  /** The ids whose module has loaded. Their panels stay in the page, hidden or shown. */
  declare loaded: ReadonlySet<string>;
  declare failed: ReadonlySet<string>;

  private readonly loads = new Map<string, Promise<void>>();

  constructor() {
    super();
    this.sections = settingsSections;
    this.requested = null;
    this.loaded = new Set();
    this.failed = new Set();
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block', 'h-full', 'min-h-0');
    this.readAddress();
    window.addEventListener('popstate', this.readAddress);
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    window.removeEventListener('popstate', this.readAddress);
  }

  protected override willUpdate(): void {
    const open = this.openSection;
    if (open) this.ensureLoaded(open);
  }

  private readonly readAddress = (): void => {
    this.requested = new URLSearchParams(window.location.search).get(SECTION_QUERY_KEY);
  };

  private get openSection(): SettingsSection | undefined {
    return this.sections.find((section) => section.id === this.requested) ?? this.sections[0];
  }

  private select(section: SettingsSection): void {
    if (this.openSection === section) return;
    this.requested = section.id;
    writeSection(section.id);
  }

  /** Imports the module of `section` once; a failure is kept until Retry asks for another try. */
  private ensureLoaded(section: SettingsSection): void {
    const { id } = section;
    if (this.loaded.has(id) || this.failed.has(id) || this.loads.has(id)) return;
    const attempt = (async () => {
      if (!TAG_NAME.test(section.tag)) {
        throw new Error(`"${section.tag}" is not a custom element name`);
      }
      await section.load();
      if (!customElements.get(section.tag)) {
        throw new Error(`Loading the "${id}" section did not define <${section.tag}>`);
      }
    })();
    this.loads.set(id, attempt);
    attempt.then(
      () => {
        this.loaded = new Set(this.loaded).add(id);
      },
      (error: unknown) => {
        console.error(`[settings] cannot load the "${id}" section`, error);
        this.loads.delete(id);
        this.failed = new Set(this.failed).add(id);
      },
    );
  }

  private retry(section: SettingsSection): void {
    const failed = new Set(this.failed);
    failed.delete(section.id);
    this.failed = failed;
    this.ensureLoaded(section);
  }

  private onTabKey(event: KeyboardEvent, index: number): void {
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    const last = this.sections.length - 1;
    const destination: Record<string, number> = {
      ArrowRight: index === last ? 0 : index + 1,
      ArrowLeft: index === 0 ? last : index - 1,
      Home: 0,
      End: last,
    };
    const target = this.sections[destination[event.key] ?? -1];
    if (!target) return;
    event.preventDefault();
    this.select(target);
    this.querySelector<HTMLElement>(`#${tabId(target)}`)?.focus();
  }

  protected override render() {
    const open = this.openSection;
    const shown = this.sections.filter(
      (section) => section === open || this.loaded.has(section.id),
    );
    return html`
      <div class="flex h-full min-h-0 flex-col">
        <header class="flex-none border-b border-base-300 px-4 pt-4 sm:px-6">
          <div class="mx-auto w-full max-w-3xl">
            <h2 class="text-xl font-semibold">Server settings</h2>
            <p class="text-sm text-base-content/70">
              Saved on the server and shared by everyone who uses it.
            </p>
            <div role="tablist" class="tabs tabs-border mt-3" aria-label="Settings sections">
              ${this.sections.map((section, index) => this.renderTab(section, index, open))}
            </div>
          </div>
        </header>
        <div class="min-h-0 flex-1 overflow-y-auto">
          <div class="mx-auto w-full max-w-3xl p-4 sm:p-6">
            ${repeat(
              shown,
              (section) => section.id,
              (section) => this.renderPanel(section, section === open),
            )}
          </div>
        </div>
      </div>
    `;
  }

  private renderTab(section: SettingsSection, index: number, open: SettingsSection | undefined) {
    const selected = section === open;
    return html`
      <button
        type="button"
        role="tab"
        id=${tabId(section)}
        class=${selected ? 'tab tab-active gap-2' : 'tab gap-2 text-base-content/70'}
        aria-selected=${selected ? 'true' : 'false'}
        aria-controls=${selected ? panelId(section) : nothing}
        tabindex=${selected ? '0' : '-1'}
        @click=${() => this.select(section)}
        @keydown=${(event: KeyboardEvent) => this.onTabKey(event, index)}
      >
        <apm-icon name=${section.icon} size="sm"></apm-icon>
        ${section.label}
      </button>
    `;
  }

  private renderPanel(section: SettingsSection, selected: boolean) {
    return html`
      <div
        role="tabpanel"
        id=${panelId(section)}
        aria-labelledby=${tabId(section)}
        ?hidden=${!selected}
      >
        ${this.renderContent(section)}
      </div>
    `;
  }

  private renderContent(section: SettingsSection) {
    if (this.loaded.has(section.id)) {
      const tag = unsafeStatic(section.tag);
      return staticHtml`<${tag}></${tag}>`;
    }
    if (this.failed.has(section.id)) {
      return html`
        <div role="alert" class="alert alert-error alert-soft">
          <apm-icon name="circle-alert"></apm-icon>
          <div>
            <h3 class="font-semibold">${section.label} could not be loaded</h3>
            <p class="text-sm">
              Check the connection to the server. If the app was just updated, reload this page.
            </p>
          </div>
          <button type="button" class="btn btn-sm" @click=${() => this.retry(section)}>Retry</button>
        </div>
      `;
    }
    return html`
      <div class="flex justify-center p-12" role="status" aria-label="Loading ${section.label}">
        <span class="loading loading-spinner loading-lg"></span>
      </div>
    `;
  }
}

customElements.define('apm-settings-page', ApmSettingsPage);

declare global {
  interface HTMLElementTagNameMap {
    'apm-settings-page': ApmSettingsPage;
  }
}
