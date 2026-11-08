import { html, nothing, type TemplateResult } from 'lit';
import { contentUrl } from '@/api/files';
import type { FileDetail } from '@/api/generated/FileDetail';
import type { FileItem } from '@/api/generated/FileItem';
import '@/components/apm-icon';
import { renderInfoTable } from '@/components/viewer-info';
import type { NavAction } from '@/components/viewer-nav';
import { positionLabel } from '@/components/viewer-nav';

/** `loading`: the list is on its way. `missing`: the file is not in it. `failed`: it could not be loaded. */
export type IdsStatus = 'loading' | 'ready' | 'missing' | 'failed';

/** What became of the request for the current file's detail. `idle`: none is needed or none is out. */
export type DetailStatus = 'idle' | 'pending' | 'failed' | 'missing';

/** Everything the viewer's chrome (toolbar, step buttons, info panel) shows, read once per render. */
export interface ChromeView {
  /** The file asked for, even while its row is not known yet. */
  id: FileItem['id'];
  /** The row to show: none while it loads, or when the server says the file is gone. */
  item: FileItem | undefined;
  /** The file's detail, once it has arrived. */
  detail: FileDetail | undefined;
  idsStatus: IdsStatus;
  /** Where `id` is in the list; only meaningful when `idsStatus` is `ready`. */
  index: number;
  /** The length of the list. */
  count: number;
  detailStatus: DetailStatus;
  zoomed: boolean;
  infoOpen: boolean;
}

export interface ChromeActions {
  toggleInfo(): void;
  toggleZoom(): void;
  step(action: Extract<NavAction, 'next' | 'previous'>): void;
  retryDetail(): void;
}

const STEP_BUTTON = 'btn btn-circle absolute top-1/2 z-10 -translate-y-1/2 border-0 bg-base-100/70';

function renderPosition({ idsStatus, index, count }: ChromeView): TemplateResult {
  switch (idsStatus) {
    case 'loading':
      return html`<div class="skeleton h-6 w-16 rounded-full" data-viewer-position aria-hidden="true"></div>`;
    case 'ready':
      return html`<span class="badge badge-neutral" data-viewer-position>${positionLabel(index, count)}</span>`;
    case 'failed':
      return html`<span
        class="badge badge-warning"
        data-viewer-notice
        title="The list of files could not be loaded, so there is nothing to step to"
        >Couldn't load the list</span
      >`;
    case 'missing':
      return html`<span
        class="badge badge-warning"
        data-viewer-notice
        title="This file is not in the list any more, so there is nothing to step to"
        >Not in the list anymore</span
      >`;
  }
}

/**
 * The toolbar: the position (or why there is none), then the info and zoom toggles and the links
 * to the original. Zoom is for images only; a file the server no longer has has no links.
 */
export function renderToolbar(view: ChromeView, actions: ChromeActions): TemplateResult {
  const canZoom = view.item?.kind === 'image';
  const zoomed = canZoom && view.zoomed;
  return html`
    <div class="flex flex-wrap items-center gap-2 pb-2">
      ${renderPosition(view)}
      <div class="ml-auto flex flex-wrap items-center gap-1">
        <button
          type="button"
          class="btn btn-sm btn-ghost ${view.infoOpen ? 'btn-active' : ''}"
          aria-pressed=${view.infoOpen}
          aria-label="Info"
          title="Info (I)"
          @click=${actions.toggleInfo}
        >
          <apm-icon name="info" size="sm"></apm-icon>
          <span class="hidden sm:inline">Info</span>
        </button>
        <button
          type="button"
          class="btn btn-sm btn-ghost ${zoomed ? 'btn-active' : ''}"
          aria-pressed=${zoomed}
          aria-label="100% zoom"
          title="Zoom to 100% (Z)"
          ?disabled=${!canZoom}
          @click=${actions.toggleZoom}
        >
          <apm-icon name=${zoomed ? 'zoom-out' : 'zoom-in'} size="sm"></apm-icon>
          <span class="hidden sm:inline">100%</span>
        </button>
        ${
          view.detailStatus === 'missing'
            ? nothing
            : html`
              <a
                class="btn btn-sm btn-ghost"
                href=${contentUrl(view.id)}
                target="_blank"
                rel="noopener"
                aria-label="Open original"
                title="Open the original in a new tab"
              >
                <apm-icon name="external-link" size="sm"></apm-icon>
                <span class="hidden sm:inline">Open original</span>
              </a>
              <a
                class="btn btn-sm btn-ghost"
                href=${contentUrl(view.id, { download: true })}
                download
                aria-label="Download"
                title="Download the original"
              >
                <apm-icon name="download" size="sm"></apm-icon>
                <span class="hidden sm:inline">Download</span>
              </a>`
        }
      </div>
    </div>
  `;
}

/**
 * Previous and next, over the stage's left and right edges at its vertical middle (clear of a
 * video's control bar). Absent without a list to step through; disabled at the ends.
 */
export function renderStepButtons(
  { idsStatus, index, count }: ChromeView,
  actions: ChromeActions,
): TemplateResult | typeof nothing {
  if (idsStatus !== 'ready' || count < 2) return nothing;
  return html`
    <button
      type="button"
      class="${STEP_BUTTON} left-2"
      aria-label="Previous file"
      title="Previous (←)"
      ?disabled=${index <= 0}
      @click=${() => actions.step('previous')}
    >
      <apm-icon name="chevron-left" size="lg"></apm-icon>
    </button>
    <button
      type="button"
      class="${STEP_BUTTON} right-2"
      aria-label="Next file"
      title="Next (→)"
      ?disabled=${index >= count - 1}
      @click=${() => actions.step('next')}
    >
      <apm-icon name="chevron-right" size="lg"></apm-icon>
    </button>
  `;
}

function renderDetailAlert(
  message: string,
  tone: 'alert-error' | 'alert-warning',
  actions: ChromeActions,
): TemplateResult {
  return html`
    <div role="alert" class="alert alert-soft ${tone} alert-vertical mb-2" data-info-alert>
      <apm-icon name="circle-alert"></apm-icon>
      <span>${message}</span>
      <button type="button" class="btn btn-sm" @click=${actions.retryDetail}>
        <apm-icon name="refresh-cw" size="sm"></apm-icon>
        Retry
      </button>
    </div>
  `;
}

function renderInfoContent(view: ChromeView, actions: ChromeActions): TemplateResult {
  const { item, detail, detailStatus } = view;
  if (item) {
    return html`
      ${detailStatus === 'failed' ? renderDetailAlert('Some details could not be loaded', 'alert-warning', actions) : nothing}
      ${renderInfoTable({ item, detail, pending: detailStatus === 'pending' })}
    `;
  }
  switch (detailStatus) {
    case 'missing':
      return html`<p class="p-2" data-info-missing>This file is no longer available</p>`;
    case 'failed':
      return renderDetailAlert("The file's details could not be loaded", 'alert-error', actions);
    default:
      return html`<div class="flex flex-col gap-3 p-2" data-info-pending aria-hidden="true">
        <div class="skeleton h-4 w-full"></div>
        <div class="skeleton h-4 w-3/4"></div>
        <div class="skeleton h-4 w-full"></div>
        <div class="skeleton h-4 w-1/2"></div>
      </div>`;
  }
}

/**
 * The info panel: a 20rem column beside the stage, over it below `md`. Absent while closed. It
 * shows the file's table, a skeleton while an unknown file loads, or why there is nothing.
 */
export function renderInfoPanel(
  view: ChromeView,
  actions: ChromeActions,
): TemplateResult | typeof nothing {
  if (!view.infoOpen) return nothing;
  return html`<aside
    aria-label="File information"
    class="absolute inset-y-0 right-0 z-20 w-80 max-w-full overflow-y-auto border-l border-base-300 bg-base-100 p-3 md:static md:z-auto md:shrink-0"
  >
    ${renderInfoContent(view, actions)}
  </aside>`;
}
