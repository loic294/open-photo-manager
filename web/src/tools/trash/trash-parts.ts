import { html, nothing } from 'lit';
import type { TrashVolumeSummary } from '@/api/generated/TrashVolumeSummary';
import { tooltip } from '@/components/tooltip';
import { itemsLabel, rangeText, reasonStyle } from '@/tools/trash/trash-text';
import { formatFileSize } from '@/util/format';
import '@/components/opm-icon';
import '@/components/opm-select';
import type { SelectChangeEvent, SelectOption } from '@/components/opm-select';

/** Why an item was trashed, as a badge; the tooltip says it in a sentence. */
export function renderReasonBadge(reason: string | null) {
  const { label, badge, hint } = reasonStyle(reason);
  return html`<span class="badge badge-sm ${badge}" ${tooltip(hint)}>${label}</span>`;
}

/** The entries of the volume filter: every volume, then the volumes that hold trash. */
export function volumeOptions(volumes: readonly TrashVolumeSummary[]): SelectOption[] {
  return [
    { value: '', label: 'All volumes' },
    ...volumes.map((volume) => ({
      value: String(volume.volume_id),
      label: volume.online ? volume.name : `${volume.name} (offline)`,
    })),
  ];
}

/** The volume filter of both tabs. `onChange` gets the volume id, `null` for all volumes. */
export function renderVolumeSelect(
  volumes: readonly TrashVolumeSummary[],
  volumeId: number | null,
  onChange: (volumeId: number | null) => void,
) {
  return html`
    <opm-select
      label="Volume"
      size="sm"
      .options=${volumeOptions(volumes)}
      .value=${volumeId === null ? '' : String(volumeId)}
      @select-change=${(event: SelectChangeEvent) => {
        event.stopPropagation();
        onChange(event.detail.value === '' ? null : Number(event.detail.value));
      }}
    ></opm-select>
  `;
}

/** One badge per volume that holds trash: its size, and a note when its items cannot be changed. */
export function renderVolumeBadges(volumes: readonly TrashVolumeSummary[]) {
  return html`
    <ul class="flex flex-wrap gap-2" aria-label="Volumes with trash">
      ${volumes.map((volume) => {
        const note = !volume.online ? 'offline' : !volume.writable ? 'read-only' : null;
        const label = [volume.name, formatFileSize(volume.bytes), note]
          .filter((part) => part !== null)
          .join(' \u00b7 ');
        return html`
          <li
            class=${note === null ? 'badge badge-outline' : 'badge badge-outline badge-warning gap-1'}
            ${tooltip(itemsLabel(volume.items))}
          >
            ${
              note === null
                ? nothing
                : html`<opm-icon name=${volume.online ? 'lock' : 'wifi-off'} size="xs"></opm-icon>`
            }
            ${label}
          </li>
        `;
      })}
    </ul>
  `;
}

export interface PagerState {
  offset: number;
  limit: number;
  total: number;
  /** How many rows the page shows. */
  count: number;
}

/** The range of the page and the previous and next buttons. `onPage` gets the new offset. */
export function renderPager(state: PagerState, onPage: (offset: number) => void) {
  const { offset, limit, total, count } = state;
  return html`
    <nav class="flex items-center justify-between gap-3 px-1 py-3" aria-label="Pages">
      <span class="text-sm text-base-content/70" aria-live="polite">
        ${rangeText(offset, count, total)}
      </span>
      <div class="join">
        <button
          type="button"
          class="btn btn-sm join-item"
          aria-label="Previous page"
          ${tooltip('Previous page')}
          ?disabled=${offset <= 0}
          @click=${() => onPage(Math.max(0, offset - limit))}
        >
          <opm-icon name="chevron-left" size="sm"></opm-icon>
        </button>
        <button
          type="button"
          class="btn btn-sm join-item"
          aria-label="Next page"
          ${tooltip('Next page')}
          ?disabled=${offset + limit >= total}
          @click=${() => onPage(offset + limit)}
        >
          <opm-icon name="chevron-right" size="sm"></opm-icon>
        </button>
      </div>
    </nav>
  `;
}

/** Placeholder rows for the first load of a tab. */
export function renderSkeleton(label: string) {
  return html`
    <div class="flex flex-col gap-3 py-4" role="status" aria-label=${label}>
      <div class="skeleton h-8 w-full"></div>
      <div class="skeleton h-8 w-full"></div>
      <div class="skeleton h-8 w-full"></div>
      <div class="skeleton h-8 w-full"></div>
    </div>
  `;
}

/**
 * What a tab shows when it has no row: an empty trash (with `hint`, the retention rule), or
 * filters that match nothing.
 */
export function renderEmpty(filtered: boolean, onClear: () => void, hint = '') {
  if (filtered) {
    return html`
      <div class="flex flex-col items-center gap-2 px-4 py-12 text-center" role="status">
        <opm-icon name="funnel-x" size="lg"></opm-icon>
        <p class="font-medium">Nothing matches</p>
        <p class="text-sm text-base-content/70">No item in the trash matches these filters.</p>
        <button type="button" class="btn btn-sm mt-2" @click=${onClear}>Clear filters</button>
      </div>
    `;
  }
  return html`
    <div class="flex flex-col items-center gap-2 px-4 py-12 text-center" role="status">
      <opm-icon name="trash-2" size="lg"></opm-icon>
      <p class="font-medium">The trash is empty</p>
      ${hint === '' ? nothing : html`<p class="text-sm text-base-content/70">${hint}</p>`}
    </div>
  `;
}
