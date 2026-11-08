import { html, nothing } from 'lit';
import { repeat } from 'lit/directives/repeat.js';
import type { FolderEntry } from '@/api/generated/FolderEntry';
import { ApmElement } from '@/base/apm-element';
import type { IconName } from '@/components/apm-icon';
import '@/components/apm-icon';
import type { PaneId } from '@/stores/nav';
import { type FolderState, folderState } from '@/util/folder-state';
import { formatCount } from '@/util/format-count';
import { isPlainClick } from '@/util/plain-click';

export interface FolderOpenDetail {
  entry: FolderEntry;
}

export type FolderOpenEvent = CustomEvent<FolderOpenDetail>;

declare global {
  interface GlobalEventHandlersEventMap {
    'folder-open': FolderOpenEvent;
  }
}

/** Placeholder rows shown while the first answer of a listing is awaited. */
const SKELETON_ROWS = [0, 1, 2, 3];

const LIST_LABEL: Readonly<Record<PaneId, string>> = {
  source: 'Source folders',
  comparison: 'Comparison folders',
};

/**
 * `content-visibility: auto` lets the browser skip the layout and paint of the rows that are off
 * screen, so hundreds of folders stay cheap without a virtualiser. It needs an intrinsic size to
 * keep the scrollbar steady: the real row height (2.1875rem = a 21px line, 12px of padding and a
 * 2px border), or the scroll height drifts while the rows are scrolled into view.
 */
const ITEM_CLASS = '[content-visibility:auto] [contain-intrinsic-size:auto_2.1875rem]';

/**
 * `content-visibility: auto` implies paint containment, which clips an outline drawn outside the
 * row: the focus ring is drawn inside. A transparent border keeps every row the same height as
 * the plan's dashed and red ones.
 */
const ROW_CLASS =
  'list-row items-center gap-2 border px-2 py-1.5 hover:bg-base-200 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-primary';

function iconFor(entry: FolderEntry): IconName {
  if (entry.overlay.will_delete) return 'folder-x';
  if (entry.overlay.planned) return 'folder-plus';
  return entry.path === '' ? 'hard-drive' : 'folder';
}

/** The border and text colour of a row: the plan's states are never told by colour alone. */
function toneClass(entry: FolderEntry, state: FolderState): string {
  const classes: string[] = [];
  if (entry.overlay.will_delete) classes.push('border-error', 'text-error');
  else if (entry.overlay.planned) classes.push('border-dashed', 'border-info');
  else classes.push('border-transparent');
  if (state === 'grey') classes.push('opacity-50');
  return classes.join(' ');
}

/** `12 / 40` when the filter hides some of the items, else `40`. */
function countText(entry: FolderEntry): string {
  if (entry.count === entry.total) return formatCount(entry.count);
  return `${formatCount(entry.count)} / ${formatCount(entry.total)}`;
}

function countSentence(entry: FolderEntry): string {
  const items = (n: number) => `${formatCount(n)} ${n === 1 ? 'item' : 'items'}`;
  if (entry.count === entry.total) return items(entry.count);
  return `${formatCount(entry.count)} of ${items(entry.total)}`;
}

function matchesText(matches: number): string {
  return `${formatCount(matches)} ${matches === 1 ? 'match' : 'matches'}`;
}

/**
 * The subfolders of a folder as a list, one row per folder: its icon, its name, then on the right
 * how many items are below it. The pane (`apm-pane`) owns the data and the navigation; this list
 * only shows them.
 *
 *   html`<apm-folder-list
 *     pane="comparison"
 *     .folders=${controller.folders}
 *     .hrefFor=${(entry) => linkToLocation('comparison', entryLocation(entry))}
 *     ?hide-unmatched=${hideUnmatched}
 *     ?show-matches=${plan !== null}
 *     @folder-open=${(e: FolderOpenEvent) => open(e.detail.entry)}
 *   ></apm-folder-list>`
 *
 * Properties:
 * - `folders`: the entries of the listing. A volumes root lists the volumes (`path` is `''`).
 * - `hideUnmatched` (attribute `hide-unmatched`): folders without matches are left out; the others
 *   without matches are greyed. The rule is `folderState`: a folder of the plan is never hidden.
 * - `showMatches` (attribute `show-matches`): show the plan matches of the folders that have them.
 * - `pane`: only for the accessible name of the list.
 * - `hrefFor`: the real address of a row. Optional: a row without one links to `#`.
 * - `loading`: skeleton rows, but only while there are no folders to show yet: a refresh keeps the
 *   rows on screen.
 *
 * Event `folder-open` (bubbles, `detail: { entry }`): a plain left click on a row. The click is
 * claimed (`preventDefault`) so the page does not reload; a middle click, a click with a modifier
 * key and "open in a new tab" are left to the browser and use the row's real `href`. A row is one
 * anchor and so one tab stop; Enter on it is a click.
 *
 * An empty list renders nothing: the pane owns the empty state.
 */
export class ApmFolderList extends ApmElement {
  static override properties = {
    folders: { attribute: false },
    hideUnmatched: { type: Boolean, attribute: 'hide-unmatched' },
    showMatches: { type: Boolean, attribute: 'show-matches' },
    pane: { type: String },
    hrefFor: { attribute: false },
    loading: { type: Boolean },
  };

  declare folders: readonly FolderEntry[];
  declare hideUnmatched: boolean;
  declare showMatches: boolean;
  declare pane: PaneId;
  declare hrefFor: ((entry: FolderEntry) => string) | undefined;
  declare loading: boolean;

  constructor() {
    super();
    this.folders = [];
    this.hideUnmatched = false;
    this.showMatches = false;
    this.pane = 'source';
    this.hrefFor = undefined;
    this.loading = false;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block');
  }

  private onClick(event: MouseEvent): void {
    if (!isPlainClick(event)) return;
    const target = event.target instanceof Element ? event.target : null;
    const anchor = target?.closest<HTMLAnchorElement>('a[data-name]');
    if (!anchor || !this.contains(anchor)) return;
    const entry = this.folders.find((folder) => folder.name === anchor.dataset.name);
    if (!entry) return;
    event.preventDefault();
    this.dispatchEvent(
      new CustomEvent<FolderOpenDetail>('folder-open', { detail: { entry }, bubbles: true }),
    );
  }

  private renderSkeleton() {
    return html`
      <div role="status">
        <span class="sr-only">Loading folders</span>
        <div aria-hidden="true" class="flex flex-col gap-1 px-2 py-1">
          ${SKELETON_ROWS.map(() => html`<div class="skeleton h-7 w-full"></div>`)}
        </div>
      </div>
    `;
  }

  private renderRow(entry: FolderEntry, state: FolderState) {
    const { overlay, matches } = entry;
    const href = this.hrefFor ? this.hrefFor(entry) : '#';
    return html`
      <li class=${ITEM_CLASS}>
        <a href=${href} data-name=${entry.name} class="${ROW_CLASS} ${toneClass(entry, state)}">
          <apm-icon name=${iconFor(entry)} size="sm"></apm-icon>
          <span class="list-col-grow min-w-0 truncate" title=${entry.name}>${entry.name}</span>
          <span class="flex shrink-0 items-center gap-1">
            ${overlay.will_delete ? html`<span class="sr-only">will be deleted</span>` : nothing}
            ${
              overlay.planned
                ? html`<span class="badge badge-info badge-sm" title="Will be created">new</span>`
                : nothing
            }
            <span class="badge badge-ghost badge-sm" aria-hidden="true">${countText(entry)}</span>
            <span class="sr-only">${countSentence(entry)}</span>
            ${this.showMatches && matches !== null ? this.renderMatches(matches) : nothing}
          </span>
        </a>
      </li>
    `;
  }

  private renderMatches(matches: number) {
    const tone = matches === 0 ? 'badge-ghost' : 'badge-success';
    return html`<span class="badge ${tone} badge-sm">${matchesText(matches)}</span>`;
  }

  protected override render() {
    const rows: { entry: FolderEntry; state: FolderState }[] = [];
    for (const entry of this.folders) {
      const state = folderState(entry, this.hideUnmatched);
      if (state !== 'hide') rows.push({ entry, state });
    }
    if (rows.length === 0) {
      return this.loading && this.folders.length === 0 ? this.renderSkeleton() : nothing;
    }
    return html`
      <ul class="list" role="list" aria-label=${LIST_LABEL[this.pane] ?? 'Folders'} @click=${this.onClick}>
        ${repeat(
          rows,
          ({ entry }) => entry.name,
          ({ entry, state }) => this.renderRow(entry, state),
        )}
      </ul>
    `;
  }
}

customElements.define('apm-folder-list', ApmFolderList);

declare global {
  interface HTMLElementTagNameMap {
    'apm-folder-list': ApmFolderList;
  }
}
