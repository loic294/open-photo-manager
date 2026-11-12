import { html, nothing } from 'lit';
import { ifDefined } from 'lit/directives/if-defined.js';
import { repeat } from 'lit/directives/repeat.js';
import type { SortOrder } from '@/api/generated/SortOrder';
import type { TrashItemDto } from '@/api/generated/TrashItemDto';
import type { TrashSort } from '@/api/generated/TrashSort';
import type { TrashVolumeSummary } from '@/api/generated/TrashVolumeSummary';
import { OpmElement } from '@/base/opm-element';
import {
  renderEmpty,
  renderPager,
  renderReasonBadge,
  renderSkeleton,
  renderVolumeSelect,
} from '@/tools/trash/trash-parts';
import { blockedReason, relativeTime } from '@/tools/trash/trash-text';
import { formatFileSize, formatFileTime } from '@/util/format';
import '@/components/opm-icon';

export interface TrashIdsDetail {
  ids: number[];
}
export interface TrashSelectDetail {
  id: number;
  selected: boolean;
}
export interface TrashSelectPageDetail {
  selected: boolean;
}
export interface TrashSortDetail {
  sort: TrashSort;
}
export interface TrashSearchDetail {
  value: string;
}
export interface TrashPageDetail {
  offset: number;
}
export interface TrashVolumeDetail {
  /** `null`: all volumes. */
  volumeId: number | null;
}

declare global {
  interface GlobalEventHandlersEventMap {
    'trash-clear-batch': CustomEvent<undefined>;
    'trash-clear-filters': CustomEvent<undefined>;
    'trash-page': CustomEvent<TrashPageDetail>;
    'trash-restore-items': CustomEvent<TrashIdsDetail>;
    'trash-search': CustomEvent<TrashSearchDetail>;
    'trash-select': CustomEvent<TrashSelectDetail>;
    'trash-select-page': CustomEvent<TrashSelectPageDetail>;
    'trash-sort': CustomEvent<TrashSortDetail>;
    'trash-volume': CustomEvent<TrashVolumeDetail>;
  }
}

/** The sortable columns, in the order of the table: the header text and the `sort` key. */
const SORT_COLUMNS = {
  name: 'Name',
  path: 'Original folder',
  size: 'Size',
  trashed_at: 'Deleted',
} as const satisfies Record<TrashSort, string>;

/**
 * The "Items" tab of the Trash page: a toolbar (search, volume, the deletion being shown, "Restore
 * selected"), the table of the items of the page and the pager. Presentational: the page owns the
 * data, the selection and every request.
 *
 * Properties: `items` (the page), `volumes` (the volumes that hold trash), `selected` (ids on the
 * page), `sort` and `order` (the order in effect), `offset`, `limit` and `total` (the pager),
 * `search` (the text in the box), `volumeId`, `batchLabel` (the chip of the deletion being shown,
 * `null` for none), `filtered` (something narrows the list: tells an empty trash from a filter
 * that matches nothing), `emptyHint` (the line under "The trash is empty": the retention rule),
 * `loading` (the first load: placeholders instead of rows) and `busy` (a request is on its way:
 * the restore buttons wait).
 *
 * Events (bubble): `trash-search` `{ value }` on every keystroke (the page debounces), `trash-volume`
 * `{ volumeId }`, `trash-clear-batch`, `trash-clear-filters`, `trash-sort` `{ sort }`, `trash-select`
 * `{ id, selected }`, `trash-select-page` `{ selected }`, `trash-restore-items` `{ ids }` (one row,
 * or the selection) and `trash-page` `{ offset }`.
 *
 * Rows on an offline or read-only volume cannot be selected or restored: the checkbox and the
 * button are disabled, and their `title` says why.
 */
export class OpmTrashItems extends OpmElement {
  static override properties = {
    items: { attribute: false },
    volumes: { attribute: false },
    selected: { attribute: false },
    sort: { type: String },
    order: { type: String },
    offset: { type: Number },
    limit: { type: Number },
    total: { type: Number },
    search: { type: String },
    volumeId: { attribute: false },
    batchLabel: { attribute: false },
    filtered: { type: Boolean },
    emptyHint: { type: String },
    loading: { type: Boolean },
    busy: { type: Boolean },
  };

  declare items: readonly TrashItemDto[];
  declare volumes: readonly TrashVolumeSummary[];
  declare selected: ReadonlySet<number>;
  declare sort: TrashSort;
  declare order: SortOrder;
  declare offset: number;
  declare limit: number;
  declare total: number;
  declare search: string;
  declare volumeId: number | null;
  declare batchLabel: string | null;
  declare filtered: boolean;
  declare emptyHint: string;
  declare loading: boolean;
  declare busy: boolean;

  constructor() {
    super();
    this.items = [];
    this.volumes = [];
    this.selected = new Set();
    this.sort = 'trashed_at';
    this.order = 'desc';
    this.offset = 0;
    this.limit = 50;
    this.total = 0;
    this.search = '';
    this.volumeId = null;
    this.batchLabel = null;
    this.filtered = false;
    this.emptyHint = '';
    this.loading = false;
    this.busy = false;
  }

  private emit(name: keyof GlobalEventHandlersEventMap, detail?: unknown): void {
    this.dispatchEvent(new CustomEvent(name, { detail, bubbles: true }));
  }

  /** Why the item cannot be restored now, `null` when it can. */
  private blocked(item: TrashItemDto): string | null {
    const volume = this.volumes.find((candidate) => candidate.volume_id === item.volume_id);
    return blockedReason(volume, 'restore');
  }

  private get selectedIds(): number[] {
    return this.items.filter((item) => this.selected.has(item.id)).map((item) => item.id);
  }

  private renderToolbar() {
    const count = this.selectedIds.length;
    return html`
      <div class="flex flex-wrap items-center gap-2 py-3">
        <label class="input input-sm w-full sm:w-64">
          <opm-icon name="search" size="sm"></opm-icon>
          <input
            type="search"
            class="grow"
            placeholder="Search by path"
            aria-label="Search the trash"
            .value=${this.search}
            @input=${(event: Event) =>
              this.emit('trash-search', { value: (event.target as HTMLInputElement).value })}
          />
        </label>
        ${renderVolumeSelect(this.volumes, this.volumeId, (volumeId) =>
          this.emit('trash-volume', { volumeId }),
        )}
        ${
          this.batchLabel === null
            ? nothing
            : html`
                <span class="badge badge-soft badge-lg gap-1 pr-1">
                  ${this.batchLabel}
                  <button
                    type="button"
                    class="btn btn-circle btn-ghost btn-xs"
                    aria-label="Show every deletion"
                    title="Show every deletion"
                    @click=${() => this.emit('trash-clear-batch')}
                  >
                    <opm-icon name="x" size="xs"></opm-icon>
                  </button>
                </span>
              `
        }
        <button
          type="button"
          class="btn btn-sm btn-primary ml-auto"
          ?disabled=${count === 0 || this.busy}
          @click=${() => this.emit('trash-restore-items', { ids: this.selectedIds })}
        >
          <opm-icon name="undo-2" size="sm"></opm-icon>
          Restore selected (${count})
        </button>
      </div>
    `;
  }

  private renderSortHeader(key: TrashSort) {
    const active = this.sort === key;
    const ascending = this.order === 'asc';
    const ariaSort = active ? (ascending ? 'ascending' : 'descending') : 'none';
    const icon = active ? (ascending ? 'arrow-up' : 'arrow-down') : 'arrow-up-down';
    return html`
      <th scope="col" class=${key === 'size' ? 'text-right' : ''} aria-sort=${ariaSort}>
        <button
          type="button"
          class="btn btn-ghost btn-xs gap-1 font-semibold"
          @click=${() => this.emit('trash-sort', { sort: key })}
        >
          ${SORT_COLUMNS[key]}
          <opm-icon name=${icon} size="xs"></opm-icon>
        </button>
      </th>
    `;
  }

  private renderRow(item: TrashItemDto) {
    const blocked = this.blocked(item);
    const checked = this.selected.has(item.id);
    return html`
      <tr class=${checked ? 'bg-base-200' : ''}>
        <td class="w-8">
          <input
            type="checkbox"
            class="checkbox checkbox-sm"
            aria-label=${`Select ${item.name}`}
            title=${ifDefined(blocked ?? undefined)}
            .checked=${checked}
            ?disabled=${blocked !== null}
            @change=${(event: Event) =>
              this.emit('trash-select', {
                id: item.id,
                selected: (event.target as HTMLInputElement).checked,
              })}
          />
        </td>
        <td>
          <div class="flex max-w-xs items-center gap-2">
            <opm-icon name=${item.is_dir ? 'folder' : 'file'} size="sm"></opm-icon>
            <span class="truncate" title=${item.name}>${item.name}</span>
          </div>
        </td>
        <td>
          <div class="max-w-xs truncate" title=${item.dir}>
            ${item.dir === '' ? html`<span class="text-base-content/60">Volume root</span>` : item.dir}
          </div>
        </td>
        <td>${item.volume}</td>
        <td class="whitespace-nowrap text-right tabular-nums">${formatFileSize(item.size)}</td>
        <td class="whitespace-nowrap">
          <span title=${formatFileTime(item.trashed_at)}>${relativeTime(item.trashed_at)}</span>
        </td>
        <td>${renderReasonBadge(item.reason)}</td>
        <td class="whitespace-nowrap text-right">
          <span class="inline-block" title=${ifDefined(blocked ?? undefined)}>
            <button
              type="button"
              class="btn btn-sm btn-ghost"
              title=${ifDefined(blocked ?? undefined)}
              ?disabled=${blocked !== null || this.busy}
              @click=${() => this.emit('trash-restore-items', { ids: [item.id] })}
            >
              <opm-icon name="undo-2" size="sm"></opm-icon>
              Restore
            </button>
          </span>
        </td>
      </tr>
    `;
  }

  private renderTable() {
    const selectable = this.items.filter((item) => this.blocked(item) === null);
    const chosen = selectable.filter((item) => this.selected.has(item.id)).length;
    const all = selectable.length > 0 && chosen === selectable.length;
    return html`
      <div class="overflow-x-auto">
        <table class="table table-sm">
          <caption class="sr-only">
            Items in the trash
          </caption>
          <thead>
            <tr>
              <th scope="col" class="w-8">
                <input
                  type="checkbox"
                  class="checkbox checkbox-sm"
                  aria-label="Select all items on this page"
                  .checked=${all}
                  .indeterminate=${chosen > 0 && !all}
                  ?disabled=${selectable.length === 0}
                  @change=${(event: Event) =>
                    this.emit('trash-select-page', {
                      selected: (event.target as HTMLInputElement).checked,
                    })}
                />
              </th>
              ${this.renderSortHeader('name')} ${this.renderSortHeader('path')}
              <th scope="col">Volume</th>
              ${this.renderSortHeader('size')} ${this.renderSortHeader('trashed_at')}
              <th scope="col">Reason</th>
              <th scope="col"><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            ${repeat(
              this.items,
              (item) => item.id,
              (item) => this.renderRow(item),
            )}
          </tbody>
        </table>
      </div>
      ${renderPager(
        { offset: this.offset, limit: this.limit, total: this.total, count: this.items.length },
        (offset) => this.emit('trash-page', { offset }),
      )}
    `;
  }

  private renderBody() {
    if (this.loading) return renderSkeleton('Loading items');
    if (this.items.length === 0) {
      return renderEmpty(this.filtered, () => this.emit('trash-clear-filters'), this.emptyHint);
    }
    return this.renderTable();
  }

  protected override render() {
    return html`${this.renderToolbar()}${this.renderBody()}`;
  }
}

customElements.define('opm-trash-items', OpmTrashItems);

declare global {
  interface HTMLElementTagNameMap {
    'opm-trash-items': OpmTrashItems;
  }
}
