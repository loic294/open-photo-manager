import { html, nothing, type TemplateResult } from 'lit';
import type { CaptureSource } from '@/api/generated/CaptureSource';
import type { FileDetail } from '@/api/generated/FileDetail';
import type { FileItem } from '@/api/generated/FileItem';
import type { FileKind } from '@/api/generated/FileKind';
import { isBoolean, persistedSignal } from '@/stores/persisted';
import { volumes } from '@/stores/volumes';
import {
  EM_DASH,
  formatCaptureDate,
  formatDuration,
  formatFileSize,
  formatFileTime,
} from '@/util/format';

/** Whether the viewer's info panel is open. It stays as the user left it across files and reloads. */
export const viewerInfoOpen = persistedSignal<boolean>('viewerInfoOpen', false, isBoolean);

export type InfoValue = TemplateResult | string;

export interface InfoContext {
  /** The file shown: the listing row, or `detail.item` once the detail arrived. */
  item: FileItem;
  /** Hash, mime type, birth time, tags and sidecars are only in the detail. */
  detail: FileDetail | undefined;
  /** The detail is on its way: the cells that need it show a skeleton. */
  pending: boolean;
}

export interface InfoRow {
  /** Stable name of the row (its `data-info-row`). */
  id: string;
  label: string;
  /** The cell. `null` leaves the row out: there is nothing to say about this file. */
  value(ctx: InfoContext): InfoValue | null;
}

const KIND_LABEL: Record<FileKind, string> = {
  image: 'Image',
  video: 'Video',
  audio: 'Audio',
  sidecar: 'Sidecar',
  other: 'Other',
};

const CAPTURE_SOURCE_LABEL: Record<CaptureSource, string> = {
  exif: 'EXIF',
  xmp: 'XMP',
  video: 'Video',
  fs: 'File time',
};

const RATING_MAX = 5;
const STARS = Array.from({ length: RATING_MAX }, (_, index) => index + 1);
const MIN_MEGAPIXELS_SHOWN = 0.1;
const HASH_SHOWN_CHARS = 12;

/** What a cell shows for a value that only the detail has. */
function fromDetail(ctx: InfoContext, render: (detail: FileDetail) => InfoValue): InfoValue {
  if (ctx.detail) return render(ctx.detail);
  return ctx.pending ? html`<div class="skeleton h-4 w-24" data-info-pending></div>` : EM_DASH;
}

function volumeName(volumeId: number): string {
  return volumes.value.find((volume) => volume.id === volumeId)?.name ?? `Volume ${volumeId}`;
}

function dimensions(item: FileItem): string {
  const { width, height } = item;
  if (width === null || height === null) return EM_DASH;
  const megapixels = (width * height) / 1_000_000;
  const size = `${width} \u00d7 ${height}`;
  return megapixels < MIN_MEGAPIXELS_SHOWN ? size : `${size} \u00b7 ${megapixels.toFixed(1)} MP`;
}

function capture(item: FileItem): InfoValue {
  const date = formatCaptureDate(item.capture_ts);
  if (date === EM_DASH || item.capture_src === null) return date;
  return html`${date} <span class="badge badge-ghost badge-xs">${CAPTURE_SOURCE_LABEL[item.capture_src]}</span>`;
}

export function renderRating(item: FileItem): InfoValue {
  const stars = Math.min(RATING_MAX, Math.max(0, Math.round(item.rating)));
  if (stars === 0) return 'Not rated';
  return html`<div class="rating rating-xs" role="img" aria-label=${`${stars} out of ${RATING_MAX} stars`}>
    ${STARS.map((star) => html`<div class="mask mask-star-2 bg-warning" aria-current=${star === stars ? 'true' : nothing}></div>`)}
  </div>`;
}

export function renderFlag(item: FileItem): InfoValue {
  if (item.flag > 0) return html`<span class="badge badge-success badge-sm">Selected</span>`;
  if (item.flag < 0) return html`<span class="badge badge-error badge-sm">Rejected</span>`;
  return 'None';
}

function hash(value: string | null): InfoValue {
  if (value === null) return html`<span class="text-base-content/60">not computed yet</span>`;
  const shown =
    value.length > HASH_SHOWN_CHARS ? `${value.slice(0, HASH_SHOWN_CHARS)}\u2026` : value;
  // A native tooltip: the panel scrolls, so a CSS one would be clipped at its edge.
  return html`<span class="font-mono text-xs" title=${value}>${shown}</span>`;
}

/**
 * Every row of the info panel, in display order. This is the one place to add a line: push one
 * more entry. Rows take what they need from the context and read their own signals.
 */
export const INFO_ROWS: readonly InfoRow[] = [
  { id: 'name', label: 'Name', value: ({ item }) => item.name },
  {
    id: 'location',
    label: 'Location',
    value: ({ item }) => {
      const volume = volumeName(item.volume_id);
      return item.dir === '' ? volume : `${volume}/${item.dir}`;
    },
  },
  {
    id: 'size',
    label: 'Size',
    value: ({ item }) =>
      html`<span title=${`${item.size.toLocaleString()} bytes`}>${formatFileSize(item.size)}</span>`,
  },
  {
    id: 'kind',
    label: 'Type',
    value: ({ item, detail }) =>
      detail?.mime ? `${KIND_LABEL[item.kind]} \u00b7 ${detail.mime}` : KIND_LABEL[item.kind],
  },
  { id: 'dimensions', label: 'Dimensions', value: ({ item }) => dimensions(item) },
  {
    id: 'duration',
    label: 'Duration',
    value: ({ item }) => (item.duration_ms === null ? null : formatDuration(item.duration_ms)),
  },
  { id: 'captured', label: 'Captured', value: ({ item }) => capture(item) },
  { id: 'camera', label: 'Camera', value: ({ item }) => item.camera ?? EM_DASH },
  { id: 'rating', label: 'Rating', value: ({ item }) => renderRating(item) },
  { id: 'flag', label: 'Flag', value: ({ item }) => renderFlag(item) },
  {
    id: 'tags',
    label: 'Tags',
    value: (ctx) =>
      fromDetail(ctx, ({ tags }) =>
        tags.length === 0
          ? EM_DASH
          : html`<div class="flex flex-wrap gap-1">${tags.map((tag) => html`<span class="badge badge-neutral badge-sm">${tag}</span>`)}</div>`,
      ),
  },
  {
    id: 'sidecars',
    label: 'Sidecars',
    value: (ctx) =>
      fromDetail(ctx, ({ sidecars }) =>
        sidecars.length === 0
          ? EM_DASH
          : html`<ul class="space-y-1">${sidecars.map((sidecar) => html`<li>${sidecar.name} <span class="badge badge-ghost badge-xs">${sidecar.kind}</span></li>`)}</ul>`,
      ),
  },
  { id: 'hash', label: 'Hash', value: (ctx) => fromDetail(ctx, (detail) => hash(detail.hash)) },
  { id: 'modified', label: 'Modified', value: ({ item }) => formatFileTime(item.mtime) },
  {
    id: 'created',
    label: 'Created',
    value: (ctx) => fromDetail(ctx, (detail) => formatFileTime(detail.btime)),
  },
  {
    id: 'immich',
    label: 'Immich',
    value: ({ item }) =>
      item.in_immich ? html`<span class="badge badge-info badge-sm">In Immich</span>` : null,
  },
];

/** The info panel's table: one row per `INFO_ROWS` entry that has something to say. */
export function renderInfoTable(ctx: InfoContext): TemplateResult {
  return html`<table class="table table-sm">
    <tbody>
      ${INFO_ROWS.map((row) => {
        const value = row.value(ctx);
        if (value === null) return nothing;
        return html`<tr data-info-row=${row.id}>
          <th scope="row" class="w-24 align-top font-normal whitespace-nowrap text-base-content/60">${row.label}</th>
          <td class="break-all">${value}</td>
        </tr>`;
      })}
    </tbody>
  </table>`;
}
