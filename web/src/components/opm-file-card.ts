import { html, nothing } from 'lit';
import { live } from 'lit/directives/live.js';
import { thumbSizeFor, thumbUrl } from '@/api/files';
import type { FileItem } from '@/api/generated/FileItem';
import type { FileKind } from '@/api/generated/FileKind';
import type { Overlay } from '@/api/generated/Overlay';
import { OpmElement } from '@/base/opm-element';
import { tooltip } from '@/components/tooltip';
import '@/components/opm-icon';
import type { IconName } from '@/components/opm-icon';
import { GRID_SIZE } from '@/stores/prefs';
import { EM_DASH, formatCaptureDate, formatDuration, formatFileSize } from '@/util/format';

/** Height in px of the caption (name and date) under the thumbnail. */
export const CARD_CAPTION_PX = 40;

/** Height in px of a card `size` px wide: the caption sits under a thumbnail area that is square. */
export function cardHeight(size: number): number {
  return size + CARD_CAPTION_PX;
}

export type CardActivateVia = 'checkbox' | 'card' | 'space' | 'enter';

/** The detail of the bubbling `card-activate` event: the card only reports, it decides nothing. */
export interface CardActivateDetail {
  id: number;
  /** The card's `index` property: its position in the grid (-1 outside a grid). */
  index: number;
  via: CardActivateVia;
  shift: boolean;
  /** Cmd (macOS) or Ctrl was held. */
  toggle: boolean;
}

export type CardActivateEvent = CustomEvent<CardActivateDetail>;

declare global {
  interface GlobalEventHandlersEventMap {
    'card-activate': CardActivateEvent;
  }
}

const BORDER_CLASS = {
  primary: 'border-primary',
  error: 'border-error',
  success: 'border-success',
  none: 'border-base-300',
} as const;

const RING_CLASS = {
  error: 'ring-2 ring-inset ring-error',
  success: 'ring-2 ring-inset ring-success',
} as const;

/** The frame of a card that is still loading. */
const SKELETON_FRAME = `${BORDER_CLASS.none} bg-base-100`;

/**
 * The classes that draw the frame of a card: its border, its background and, when a second state
 * has to stay visible, an inner ring. Pure, so the whole table is tested.
 *
 * Three states compete for the border, strongest first: selected, will delete, matched. The
 * strongest one present takes the border and the next one present, if any, an inner ring.
 *
 * | selected | will delete | matched | border          | ring    |
 * | -------- | ----------- | ------- | --------------- | ------- |
 * | yes      | yes         | any     | `border-primary`| error   |
 * | yes      | no          | yes     | `border-primary`| success |
 * | yes      | no          | no      | `border-primary`| none    |
 * | no       | yes         | yes     | `border-error`  | success |
 * | no       | yes         | no      | `border-error`  | none    |
 * | no       | no          | yes     | `border-success`| none    |
 * | no       | no          | no      | `border-base-300`| none   |
 *
 * Selected also adds a light `bg-primary/10`. A ghost (`overlay.planned`) is never selectable, so
 * it ignores `selected`; its border is dashed and the card is dimmed, and the other states colour
 * it as in the table.
 */
export function cardFrameClasses(overlay: Overlay, selected: boolean): string {
  const ghost = overlay.planned;
  const states: Array<keyof typeof RING_CLASS | 'primary'> = [];
  if (selected && !ghost) states.push('primary');
  if (overlay.will_delete) states.push('error');
  if (overlay.matched) states.push('success');
  const [border, ring] = states;
  const classes: string[] = [BORDER_CLASS[border ?? 'none']];
  if (border === 'primary') classes.push('bg-primary/10');
  else classes.push('bg-base-100');
  if (ring && ring !== 'primary') classes.push(RING_CLASS[ring]);
  if (ghost) classes.push('border-dashed', 'opacity-60');
  return classes.join(' ');
}

export type CardBadgeVariant = 'error' | 'info' | 'neutral' | 'primary' | 'success' | 'warning';

export interface CardBadge {
  /** Stable name of the badge (also its `data-badge`). */
  id: string;
  icon?: IconName;
  /** Shown next to the icon. A badge needs an icon, a text or both. */
  text?: string;
  /** Accessible name and tooltip. */
  label: string;
  variant: CardBadgeVariant;
  /** `top` is the row at the top left, `bottom` the one at the bottom left. */
  place: 'top' | 'bottom';
}

const BADGE_VARIANT_CLASS = {
  error: 'badge-error',
  info: 'badge-info',
  neutral: 'badge-neutral',
  primary: 'badge-primary',
  success: 'badge-success',
  warning: 'badge-warning',
} as const satisfies Record<CardBadgeVariant, string>;

/**
 * Every badge of a card, in display order. This is the one place to add a badge: push one more
 * entry. A plain image has none.
 */
export function cardBadges(item: FileItem): CardBadge[] {
  const badges: CardBadge[] = [];
  if (item.overlay.planned) {
    badges.push({
      id: 'new',
      text: 'new',
      label: 'New file: the plan will create it',
      variant: 'primary',
      place: 'top',
    });
  }
  if (item.rating > 0) {
    badges.push({
      id: 'rating',
      icon: 'star',
      text: String(item.rating),
      label: `${item.rating} ${item.rating === 1 ? 'star' : 'stars'}`,
      variant: 'warning',
      place: 'top',
    });
  }
  if (item.flag !== 0) {
    badges.push({
      id: 'flag',
      icon: 'flag',
      label: item.flag > 0 ? 'Flagged selected' : 'Flagged rejected',
      variant: item.flag > 0 ? 'success' : 'error',
      place: 'top',
    });
  }
  if (item.in_immich) {
    badges.push({
      id: 'immich',
      icon: 'cloud',
      label: 'In Immich',
      variant: 'info',
      place: 'top',
    });
  }
  const kind = kindBadge(item);
  if (kind) badges.push(kind);
  return badges;
}

/** The kind of a file that is not a plain image: videos and audio add their duration. */
function kindBadge(item: FileItem): CardBadge | undefined {
  const base = { id: 'kind', variant: 'neutral', place: 'bottom' } as const;
  const duration = item.duration_ms === null ? undefined : formatDuration(item.duration_ms);
  switch (item.kind) {
    case 'video':
      return { ...base, icon: 'play', text: duration, label: withDuration('Video', duration) };
    case 'audio':
      return { ...base, icon: 'music', text: duration, label: withDuration('Audio', duration) };
    case 'sidecar':
      return { ...base, icon: 'file', label: 'Sidecar file' };
    case 'other':
      return { ...base, icon: 'file', label: 'File' };
    default:
      return undefined;
  }
}

function withDuration(name: string, duration: string | undefined): string {
  return duration ? `${name}, ${duration}` : name;
}

/** The icon standing for a file kind where there is no picture of it (shared with the viewer). */
export const KIND_ICON: Record<FileKind, IconName> = {
  image: 'image',
  video: 'film',
  audio: 'music',
  sidecar: 'file',
  other: 'file',
};

/** The server renders thumbnails of images and videos only (shared with the viewer). */
export function hasThumbnail(kind: FileKind): boolean {
  return kind === 'image' || kind === 'video';
}

interface CardDate {
  text: string;
  icon: IconName | null;
  hint: string;
}

const FROM_METADATA = {
  exif: 'Capture date from the EXIF data',
  xmp: 'Capture date from the XMP data',
  video: 'Capture date from the video metadata',
} as const;

const FILE_DATE_HINT = 'File date (created or modified): the file has no capture date';

/** `capture_ts` is the photographer's wall clock; the icon tells where it comes from. */
function cardDate(item: FileItem): CardDate {
  const text = formatCaptureDate(item.capture_ts);
  if (text === EM_DASH) return { text, icon: null, hint: 'No date' };
  const source = item.capture_src;
  if (source === 'exif' || source === 'xmp' || source === 'video') {
    return { text, icon: 'camera', hint: FROM_METADATA[source] };
  }
  return { text, icon: 'clock', hint: FILE_DATE_HINT };
}

/** What a screen reader says for the card: the name, the date, then everything the badges show. */
function cardLabel(item: FileItem): string {
  const date = formatCaptureDate(item.capture_ts);
  const parts = [item.name, date === EM_DASH ? 'no date' : date];
  if (item.overlay.will_delete) parts.push('will be deleted');
  if (item.overlay.matched) parts.push('matched');
  for (const badge of cardBadges(item)) parts.push(badge.label);
  return parts.join(', ');
}

const CHECKBOX_SHOWN = 'opacity-100';

// Hidden until the card is hovered or has keyboard focus; always shown where there is no hover.
const CHECKBOX_ON_DEMAND =
  'pointer-events-none opacity-0 group-hover:pointer-events-auto group-hover:opacity-100 group-focus-visible:pointer-events-auto group-focus-visible:opacity-100 [@media(hover:none)]:pointer-events-auto [@media(hover:none)]:opacity-100';

interface ThumbState {
  /** The item the state belongs to: another item starts over. */
  id: number | null;
  /** The URL whose image is on screen. */
  shown: string | null;
  /** The URL the server could not render. */
  failed: string | null;
}

/**
 * One tile of the file grid: the thumbnail, the file name, the capture date, badges and a
 * selection checkbox. It is driven by properties only and keeps no selection state: it reports
 * what the user did with a bubbling `card-activate` event (`CardActivateDetail`) and the owner
 * decides what that means. A card without an `item` is a loading placeholder.
 *
 *   <opm-file-card .item=${item} .index=${3} ?selected=${true} size="160"></opm-file-card>
 *
 * Properties: `item` (`undefined` while it loads), `index` (reported in the event), `selected`,
 * `selectionActive` (shows the checkbox on every card), `size` (the card width in px: it picks the
 * thumbnail rendition, the owner sizes the card), `focused` (the roving tabindex target) and
 * `setSize` (the number of items in the list, for `aria-posinset` / `aria-setsize`; 0 = unknown).
 *
 * The inner `div[role=option]` is what takes the focus: call `focus()` on the card. A ghost
 * (`overlay.planned`: a file the plan will create) is dimmed and dashed, ignores `selected`, shows no
 * checkbox and never emits. See `cardFrameClasses` for the border states and `cardBadges` for the
 * badges.
 * The checkbox is a decorative pointer target within that option, not a second accessible
 * control: selection is announced by `aria-selected` and Space selects the focused option.
 */
export class OpmFileCard extends OpmElement {
  static override properties = {
    item: { attribute: false },
    index: { type: Number },
    selected: { type: Boolean },
    asButton: { type: Boolean, attribute: 'as-button' },
    highlightSelection: { type: Boolean, attribute: 'highlight-selection' },
    showCaption: { type: Boolean, attribute: 'show-caption' },
    selectionActive: { type: Boolean, attribute: 'selection-active' },
    size: { type: Number },
    focused: { type: Boolean },
    setSize: { type: Number, attribute: 'set-size' },
  };

  declare item: FileItem | undefined;
  declare index: number;
  declare selected: boolean;
  asButton = false;
  highlightSelection = true;
  showCaption = true;
  declare selectionActive: boolean;
  declare size: number;
  declare focused: boolean;
  declare setSize: number;

  private thumb: ThumbState = { id: null, shown: null, failed: null };
  private released = false;

  constructor() {
    super();
    this.item = undefined;
    this.index = -1;
    this.selected = false;
    this.selectionActive = false;
    this.size = GRID_SIZE.default;
    this.focused = false;
    this.setSize = 0;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block', 'h-full', 'w-full');
    if (this.released) {
      this.released = false;
      this.requestUpdate();
    }
  }

  /**
   * A card that leaves the DOM (the grid scrolled past it) drops its thumbnail request, so a fast
   * scroll does not queue hundreds of them behind the browser's connection limit. A card that is
   * only moved is connected again by the time the microtask runs and keeps it.
   */
  override disconnectedCallback(): void {
    super.disconnectedCallback();
    queueMicrotask(() => {
      if (this.isConnected) return;
      const image = this.querySelector('img');
      if (!image?.hasAttribute('src')) return;
      image.removeAttribute('src');
      this.thumb = { id: this.thumb.id, shown: null, failed: null };
      this.released = true;
    });
  }

  /** Focuses the card (the host itself is not focusable). */
  override focus(options?: FocusOptions): void {
    const option = this.option();
    if (option) option.focus(options);
    else void this.updateComplete.then(() => this.option()?.focus(options));
  }

  private option(): HTMLElement | null {
    return this.querySelector<HTMLElement>('[role="option"], [role="button"]');
  }

  protected override willUpdate(): void {
    const id = this.item?.id ?? null;
    if (this.thumb.id !== id) this.thumb = { id, shown: null, failed: null };
  }

  private activate(via: CardActivateVia, event: MouseEvent | KeyboardEvent): void {
    const { item } = this;
    if (!item || item.overlay.planned) return;
    this.dispatchEvent(
      new CustomEvent<CardActivateDetail>('card-activate', {
        bubbles: true,
        detail: {
          id: item.id,
          index: this.index,
          via,
          shift: event.shiftKey,
          toggle: event.metaKey || event.ctrlKey,
        },
      }),
    );
  }

  private onClick(event: MouseEvent): void {
    this.activate('card', event);
  }

  private onKeydown(event: KeyboardEvent): void {
    if (event.target !== event.currentTarget || event.altKey) return;
    if (event.key !== 'Enter' && event.key !== ' ') return;
    event.preventDefault();
    if (!event.repeat) this.activate(event.key === 'Enter' ? 'enter' : 'space', event);
  }

  private onCheckboxMousedown(event: MouseEvent): void {
    // Keeps the focus on the card instead of moving it into the checkbox.
    event.preventDefault();
  }

  private onCheckboxClick(event: MouseEvent): void {
    event.preventDefault();
    event.stopPropagation();
    this.option()?.focus({ preventScroll: true });
    this.activate('checkbox', event);
  }

  private onThumbLoad(event: Event): void {
    const url = (event.currentTarget as HTMLImageElement).getAttribute('src');
    if (url === null) return;
    this.thumb = { ...this.thumb, shown: url, failed: null };
    this.requestUpdate();
  }

  private onThumbError(event: Event): void {
    const url = (event.currentTarget as HTMLImageElement).getAttribute('src');
    if (url === null) return;
    this.thumb = { ...this.thumb, failed: url };
    this.requestUpdate();
  }

  private renderBadge(badge: CardBadge) {
    return html`<span
      class="badge badge-xs gap-0.5 ${BADGE_VARIANT_CLASS[badge.variant]}"
      data-badge=${badge.id}
      role="img"
      aria-label=${badge.label}
      ${tooltip(badge.label)}
    >${badge.icon ? html`<opm-icon name=${badge.icon} size="xs"></opm-icon>` : nothing}${badge.text ?? nothing}</span>`;
  }

  private renderThumbnail(item: FileItem) {
    const { thumb } = this;
    const url = thumbUrl(item, thumbSizeFor(this.size));
    if (!hasThumbnail(item.kind) || thumb.failed === url) {
      const name: IconName = thumb.failed === url ? 'image-off' : KIND_ICON[item.kind];
      return html`<opm-icon name=${name} size="xl" class="text-base-content/30"></opm-icon>`;
    }
    // The old picture stays until the new one has loaded; a different file starts with a skeleton.
    return html`
      <img
        class="absolute inset-0 h-full w-full object-contain"
        src=${live(url)}
        alt=""
        loading="lazy"
        decoding="async"
        draggable="false"
        @load=${this.onThumbLoad}
        @error=${this.onThumbError}
      />
      ${thumb.shown === null ? html`<div class="skeleton absolute inset-0 rounded-none" data-thumb-skeleton></div>` : nothing}
    `;
  }

  private renderCheckbox(selected: boolean) {
    const shown = this.selectionActive || selected;
    return html`
      <div
        class="absolute top-0 right-0 p-1 ${shown ? CHECKBOX_SHOWN : CHECKBOX_ON_DEMAND}"
        data-checkbox
        @mousedown=${this.onCheckboxMousedown}
        @click=${this.onCheckboxClick}
      >
        <span class="flex rounded-md bg-base-100/80 p-0.5 shadow-sm">
          <span
            class="checkbox checkbox-sm checkbox-primary"
            aria-hidden="true"
            aria-checked=${selected ? 'true' : 'false'}
          ></span>
        </span>
      </div>
    `;
  }

  private renderFigure(item: FileItem, selected: boolean) {
    const badges = cardBadges(item);
    const top = badges.filter((badge) => badge.place === 'top');
    const bottom = badges.filter((badge) => badge.place === 'bottom');
    return html`
      ${this.renderThumbnail(item)}
      ${
        top.length > 0
          ? html`<div class="absolute top-1 left-1 flex max-w-[calc(100%-2rem)] gap-1 overflow-hidden">${top.map((badge) => this.renderBadge(badge))}</div>`
          : nothing
      }
      ${
        bottom.length > 0
          ? html`<div class="absolute bottom-1 left-1 flex gap-1">${bottom.map((badge) => this.renderBadge(badge))}</div>`
          : nothing
      }
      ${item.overlay.planned ? nothing : this.renderCheckbox(selected)}
    `;
  }

  private renderCaption(item: FileItem) {
    const date = cardDate(item);
    return html`
      <div
        class="tooltip tooltip-top tooltip-start block min-w-0 text-left text-xs leading-4 font-medium before:max-w-full before:whitespace-normal before:break-all"
        data-name
        data-tip=${item.name}
      ><span class="block truncate">${item.name}</span></div>
      <div
        class="tooltip tooltip-top tooltip-start flex min-w-0 items-center gap-1 text-left text-xs leading-4 text-base-content/60 before:max-w-full before:whitespace-normal before:break-all"
        data-date
        data-tip=${`${date.text} · ${formatFileSize(item.size)} · ${date.hint}`}
      >
        ${date.icon ? html`<opm-icon name=${date.icon} size="xs"></opm-icon>` : nothing}
        <span class="truncate">${date.text}</span>
      </div>
    `;
  }

  private renderCaptionPlaceholder() {
    return html`
      <div class="skeleton h-3 w-3/4 rounded"></div>
      <div class="skeleton mt-1 h-3 w-1/2 rounded"></div>
    `;
  }

  protected override render() {
    const { item } = this;
    const ghost = item?.overlay.planned === true;
    const selected = item !== undefined && !ghost && this.selected;
    const frame = item
      ? cardFrameClasses(item.overlay, selected && this.highlightSelection)
      : SKELETON_FRAME;
    const interactive = item !== undefined && !ghost;
    const positioned = this.index >= 0 && this.setSize > 0;
    return html`
      <div
        role=${this.asButton ? 'button' : 'option'}
        class="card group h-full w-full rounded-lg border-2 hover:z-10 focus-within:z-10 ${frame} ${interactive ? 'cursor-pointer' : 'cursor-default'}"
        tabindex=${this.focused ? '0' : '-1'}
        aria-selected=${this.asButton ? nothing : String(selected)}
        aria-pressed=${this.asButton ? String(selected) : nothing}
        aria-label=${item ? cardLabel(item) : 'Loading'}
        aria-posinset=${positioned ? this.index + 1 : nothing}
        aria-setsize=${positioned ? this.setSize : nothing}
        aria-disabled=${ghost ? 'true' : nothing}
        aria-busy=${item ? nothing : 'true'}
        @click=${this.onClick}
        @keydown=${this.onKeydown}
      >
        <figure class="relative min-h-0 flex-1 overflow-hidden rounded-t-md ${item ? 'bg-base-200' : 'skeleton rounded-none'}">
          ${item ? this.renderFigure(item, selected) : nothing}
        </figure>
        ${
          this.showCaption
            ? html`<div class="flex h-10 shrink-0 flex-col justify-center px-2">
          ${item ? this.renderCaption(item) : this.renderCaptionPlaceholder()}
        </div>`
            : nothing
        }
      </div>
    `;
  }
}

customElements.define('opm-file-card', OpmFileCard);

declare global {
  interface HTMLElementTagNameMap {
    'opm-file-card': OpmFileCard;
  }
}
