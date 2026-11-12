import { html, nothing } from 'lit';
import { keyed } from 'lit/directives/keyed.js';
import { createRef, ref } from 'lit/directives/ref.js';
import { contentUrl, previewUrl, thumbUrl } from '@/api/files';
import type { FileItem } from '@/api/generated/FileItem';
import { OpmElement } from '@/base/opm-element';
import { KIND_ICON } from '@/components/opm-file-card';
import '@/components/opm-icon';
import { formatFileSize } from '@/util/format';
import { type ImageLoad, loadImage } from '@/util/load-image';

/** `loading`: nothing to look at yet. `ready`: the file is shown. `failed`: it could not be. */
export type StagePhase = 'loading' | 'ready' | 'failed';

/** Why there is no `item` to show: the file is gone (404), or its details could not be fetched. */
export type StageFailure = 'missing' | 'failed';

export interface StageSettledDetail {
  id: FileItem['id'];
}

export type StageSettledEvent = CustomEvent<StageSettledDetail>;

declare global {
  interface HTMLElementEventMap {
    /** The file has been shown, or has failed to show, so the network is free again. */
    'stage-settled': StageSettledEvent;
    /** The user pressed Retry on a `failure`. */
    'stage-retry': Event;
    /** The user double clicked the image: switch between fit and 100%. */
    'zoom-toggle': Event;
  }
}

const SPINNER =
  'loading loading-spinner loading-lg pointer-events-none absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2';

const ALERT_PLACE =
  'absolute right-4 bottom-4 left-4 mx-auto max-w-xl alert-vertical sm:alert-horizontal';

const CENTRED_COLUMN =
  'absolute inset-0 flex flex-col items-center justify-center px-16 py-6 text-center';

/**
 * What the viewer shows of one file, filling its parent (light DOM, so the parent is a flex row
 * or any positioned box). `item` is the file; the stage decides how to show its kind:
 *
 * - image: the large thumbnail (dimmed, with a spinner) at once, then `/preview` once it has been
 *   downloaded and decoded off-screen. `object-contain`: the whole image. `zoomed` shows it at
 *   100%, scrollable and starting centred; the double click on it asks for that (`zoom-toggle`).
 * - video: a `video` with the browser's controls, the thumbnail as its poster, no autoplay. A
 *   file the browser cannot play shows the poster and a warning with links to the original.
 * - audio: a large icon and an `audio` with controls; the same warning.
 * - anything else: an empty state with the name, the size and a Download link.
 *
 * `item` undefined is a file whose row is not known yet: the previous picture stays, dimmed, with
 * a spinner, unless `failure` says why it will never come.
 *
 * Events (none bubbles): `stage-settled` once per file, when it is shown or has failed (a Retry
 * starts over); `zoom-toggle`; `stage-retry`. Swapping `item` for another object with the same id
 * (the detail replacing the listing row) restarts nothing.
 *
 * Not for direct use: `openViewer()` mounts it. `release()` abandons the requests and stops
 * playback when the viewer closes; the stage is inert afterwards.
 */
export class OpmViewerStage extends OpmElement {
  static override properties = {
    item: { attribute: false },
    zoomed: { type: Boolean },
    failure: { type: String },
    phase: { state: true },
    thumbFailed: { state: true },
    src: { state: true },
  };

  declare item: FileItem | undefined;
  declare zoomed: boolean;
  declare failure: StageFailure | undefined;
  declare phase: StagePhase;
  /** The large thumbnail could not be fetched: hide the broken image until the preview comes. */
  declare thumbFailed: boolean;
  /** What the `img` shows: the thumbnail, then the preview. Empty for the kinds without one. */
  declare src: string;

  private shownId: FileItem['id'] | undefined;
  private settledId: FileItem['id'] | undefined;
  private preview: ImageLoad | undefined;
  private released = false;
  private centred = false;
  private attempt = 0;
  private readonly scroller = createRef<HTMLElement>();
  private readonly media = createRef<HTMLMediaElement>();

  constructor() {
    super();
    this.item = undefined;
    this.zoomed = false;
    this.failure = undefined;
    this.phase = 'loading';
    this.thumbFailed = false;
    this.src = '';
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add(
      'relative',
      'block',
      'min-h-0',
      'min-w-0',
      'flex-1',
      'bg-neutral',
      'text-neutral-content',
    );
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    this.release();
  }

  /**
   * The viewer closed: abandons the preview and the media request and empties a stage that was
   * still loading. What is painted stays, because the dialog is still fading out.
   */
  release(): void {
    this.released = true;
    this.preview?.cancel();
    this.preview = undefined;
    this.releaseMedia();
    if (this.phase === 'loading') this.src = '';
  }

  /** `pause()`, `removeAttribute('src')`, `load()`: the browser then aborts its Range request. */
  private releaseMedia(): void {
    const media = this.media.value;
    if (!media) return;
    media.pause();
    media.removeAttribute('src');
    media.load();
  }

  protected override willUpdate(): void {
    if (this.released) return;
    const id = this.item?.id;
    if (id === this.shownId) return;
    this.shownId = id;
    this.begin(this.item);
  }

  /**
   * Starts showing `item` from scratch. Retry does the same. Without an `item` the previous
   * picture stays where it is, dimmed.
   */
  private begin(item: FileItem | undefined): void {
    this.preview?.cancel();
    this.preview = undefined;
    this.releaseMedia();
    this.phase = 'loading';
    if (!item) return;
    this.thumbFailed = false;
    this.src = item.kind === 'image' ? thumbUrl(item, 'lg') : '';
    if (item.kind === 'image') this.startPreview(item.id);
    else if (item.kind !== 'video') this.phase = 'ready';
  }

  private startPreview(id: FileItem['id']): void {
    const url = previewUrl(id);
    const load = loadImage(url);
    this.preview = load;
    load.done.then(
      () => {
        if (this.preview !== load) return;
        this.src = url;
        this.thumbFailed = false;
        this.phase = 'ready';
      },
      () => {
        if (this.preview === load) this.phase = 'failed';
      },
    );
  }

  protected override updated(): void {
    const zoomed = this.zoomed && this.phase === 'ready';
    if (zoomed && !this.centred) this.centre();
    this.centred = zoomed;
    this.setAttribute('aria-busy', String(this.phase === 'loading' && !this.failure));
    this.announceSettled();
  }

  private announceSettled(): void {
    const id = this.item?.id;
    if (this.phase === 'loading' || id === undefined) {
      this.settledId = undefined;
      return;
    }
    if (this.settledId === id) return;
    this.settledId = id;
    this.dispatchEvent(new CustomEvent<StageSettledDetail>('stage-settled', { detail: { id } }));
  }

  /** Scrolls the zoomed picture so its middle is in the middle of the stage. */
  private centre(): void {
    const scroller = this.scroller.value;
    if (!scroller) return;
    scroller.scrollLeft = Math.max(0, (scroller.scrollWidth - scroller.clientWidth) / 2);
    scroller.scrollTop = Math.max(0, (scroller.scrollHeight - scroller.clientHeight) / 2);
  }

  private readonly onImageLoad = (): void => {
    if (this.zoomed && this.phase === 'ready') this.centre();
  };

  private readonly onImageError = (): void => {
    if (this.released) return;
    if (this.phase === 'loading') this.thumbFailed = true;
    else if (this.phase === 'ready') this.phase = 'failed';
  };

  private readonly onPosterError = (): void => {
    this.thumbFailed = true;
  };

  private readonly onImageDoubleClick = (): void => {
    if (this.phase === 'ready') this.dispatchEvent(new Event('zoom-toggle'));
  };

  private readonly onMediaReady = (): void => {
    if (!this.released && this.phase === 'loading') this.phase = 'ready';
  };

  private readonly onMediaError = (): void => {
    if (!this.released) this.phase = 'failed';
  };

  /** A new `img` element too (`attempt`), so no error left on the old one can leak into the retry. */
  private readonly retryImage = (): void => {
    if (!this.item) return;
    this.attempt += 1;
    this.begin(this.item);
  };

  private readonly retryLookup = (): void => {
    this.dispatchEvent(new Event('stage-retry'));
  };

  private renderSpinner() {
    return html`<span class=${SPINNER} data-stage-spinner aria-hidden="true"></span>`;
  }

  private renderOriginalLinks(item: FileItem) {
    return html`
      <div class="flex gap-2">
        <a class="btn btn-sm" href=${contentUrl(item.id)} target="_blank" rel="noopener">
          <opm-icon name="external-link" size="sm"></opm-icon>
          Open original
        </a>
        <a class="btn btn-sm" href=${contentUrl(item.id, { download: true })} download>
          <opm-icon name="download" size="sm"></opm-icon>
          Download
        </a>
      </div>
    `;
  }

  private renderPicture(item: FileItem | undefined) {
    const ready = this.phase === 'ready';
    const zoomed = this.zoomed && ready;
    const size = zoomed
      ? 'm-auto h-auto w-auto max-w-none shrink-0'
      : 'h-full w-full object-contain';
    const state = ready ? (zoomed ? 'cursor-zoom-out' : 'cursor-zoom-in') : 'opacity-40';
    return html`
      <div
        ${ref(this.scroller)}
        class="absolute inset-0 flex ${zoomed ? 'overflow-auto' : 'overflow-hidden'}"
        data-stage-scroller
      >
        ${keyed(
          this.attempt,
          html`<img
            class="${size} ${state} ${this.thumbFailed ? 'invisible' : ''}"
            src=${this.src || nothing}
            alt=${item?.name ?? ''}
            draggable="false"
            @load=${this.onImageLoad}
            @error=${this.onImageError}
            @dblclick=${this.onImageDoubleClick}
          />`,
        )}
      </div>
      ${this.phase === 'loading' ? this.renderSpinner() : nothing}
      ${
        this.phase === 'failed' && item
          ? html`<div role="alert" class="alert alert-error ${ALERT_PLACE}" data-stage-error>
              <opm-icon name="circle-alert"></opm-icon>
              <span>This image could not be loaded</span>
              <div class="flex gap-2">
                <button type="button" class="btn btn-sm" @click=${this.retryImage}>
                  <opm-icon name="refresh-cw" size="sm"></opm-icon>
                  Retry
                </button>
                <a class="btn btn-sm" href=${contentUrl(item.id, { download: true })} download>
                  <opm-icon name="download" size="sm"></opm-icon>
                  Download
                </a>
              </div>
            </div>`
          : nothing
      }
    `;
  }

  private renderPlaybackFailure(item: FileItem) {
    const noun = item.kind === 'video' ? 'video' : 'audio file';
    return html`
      <div role="alert" class="alert alert-warning ${ALERT_PLACE}" data-stage-error>
        <opm-icon name="triangle-alert"></opm-icon>
        <span>Your browser can't play this ${noun}</span>
        ${this.renderOriginalLinks(item)}
      </div>
    `;
  }

  private renderVideo(item: FileItem) {
    if (this.phase === 'failed') {
      return html`
        <img
          class="absolute inset-0 h-full w-full object-contain opacity-40 ${this.thumbFailed ? 'invisible' : ''}"
          src=${thumbUrl(item, 'lg')}
          alt=${item.name}
          draggable="false"
          @error=${this.onPosterError}
        />
        ${this.renderPlaybackFailure(item)}
      `;
    }
    return html`
      ${keyed(
        item.id,
        html`<video
          ${ref(this.media)}
          class="absolute inset-0 h-full w-full"
          controls
          playsinline
          preload="metadata"
          poster=${thumbUrl(item, 'lg')}
          src=${previewUrl(item.id)}
          @loadedmetadata=${this.onMediaReady}
          @error=${this.onMediaError}
        ></video>`,
      )}
      ${this.phase === 'loading' ? this.renderSpinner() : nothing}
    `;
  }

  private renderAudio(item: FileItem) {
    return html`
      <div class="${CENTRED_COLUMN} gap-6">
        <div class="[&_svg]:size-16">
          <opm-icon name=${KIND_ICON.audio} size="xl"></opm-icon>
        </div>
        ${keyed(
          item.id,
          html`<audio
            ${ref(this.media)}
            class="w-full max-w-xl"
            controls
            preload="metadata"
            src=${previewUrl(item.id)}
            @error=${this.onMediaError}
          ></audio>`,
        )}
      </div>
      ${this.phase === 'failed' ? this.renderPlaybackFailure(item) : nothing}
    `;
  }

  private renderOther(item: FileItem) {
    return html`
      <div class="${CENTRED_COLUMN} gap-3">
        <opm-icon name=${KIND_ICON[item.kind]} size="xl"></opm-icon>
        <p class="text-lg font-semibold">No preview for this kind of file</p>
        <p class="max-w-full text-sm break-all opacity-70">
          ${item.name} · ${formatFileSize(item.size)}
        </p>
        <a class="btn btn-primary" href=${contentUrl(item.id, { download: true })} download>
          <opm-icon name="download" size="sm"></opm-icon>
          Download
        </a>
      </div>
    `;
  }

  private renderFailure(failure: StageFailure) {
    if (failure === 'missing') {
      return html`
        <div class="${CENTRED_COLUMN} gap-3" data-stage-missing>
          <opm-icon name="image-off" size="xl"></opm-icon>
          <p class="text-lg font-semibold">This file is no longer available</p>
        </div>
      `;
    }
    return html`
      <div class=${CENTRED_COLUMN}>
        <div
          role="alert"
          class="alert alert-error alert-vertical sm:alert-horizontal max-w-xl"
          data-stage-error
        >
          <opm-icon name="circle-alert"></opm-icon>
          <span>This file could not be loaded</span>
          <button type="button" class="btn btn-sm" @click=${this.retryLookup}>
            <opm-icon name="refresh-cw" size="sm"></opm-icon>
            Retry
          </button>
        </div>
      </div>
    `;
  }

  protected override render() {
    const { item } = this;
    if (!item)
      return this.failure ? this.renderFailure(this.failure) : this.renderPicture(undefined);
    switch (item.kind) {
      case 'image':
        return this.renderPicture(item);
      case 'video':
        return this.renderVideo(item);
      case 'audio':
        return this.renderAudio(item);
      default:
        return this.renderOther(item);
    }
  }
}

customElements.define('opm-viewer-stage', OpmViewerStage);

declare global {
  interface HTMLElementTagNameMap {
    'opm-viewer-stage': OpmViewerStage;
  }
}
