import type { ReactiveController, ReactiveControllerHost } from 'lit';
import { fetchFolders } from '@/api/browse';
import type { ApiClient } from '@/api/client';
import { errorMessage, isAbortError, isApiError } from '@/api/errors';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import type { FolderEntry } from '@/api/generated/FolderEntry';
import type { FoldersResponse } from '@/api/generated/FoldersResponse';
import type { FooterCounts } from '@/api/generated/FooterCounts';
import { type EventsClient, type ServerEventOf, events as sharedEvents } from '@/api/sse';
import { INVALIDATE_DEBOUNCE_MS, INVALIDATE_MAX_WAIT_MS } from '@/stores/paged-items';
import { selectionContextKey } from '@/stores/selection';
import { isEnded } from '@/stores/tasks';
import { fsChangeAffects } from '@/util/fs-change';

/**
 * `idle`: no request. `loading`: nothing to show yet (the first load of a listing). `ready`: the
 * server answered, so `folders` and `counts` are current, or `missing` says the folder is gone.
 * `error`: the last attempt failed; `error` has the message.
 */
export type FoldersStatus = 'idle' | 'loading' | 'ready' | 'error';

export interface FoldersControllerOptions {
  /** Defaults to the shared `api`. */
  client?: Pick<ApiClient, 'post'>;
  /** Defaults to the shared `events`. */
  events?: Pick<EventsClient, 'on' | 'onResync'>;
  /** Quiet time after the last matching event before the refetch; defaults to `PagedItems`'. */
  debounceMs?: number;
  /** A steady stream of events cannot postpone the refetch for longer than this. */
  maxWaitMs?: number;
}

const NO_FOLDERS: readonly FolderEntry[] = Object.freeze([]);

function planKeyOf(request: BrowseRequest): string | null {
  return request.plan === null ? null : `${request.plan.plan_id}:${request.plan.side}`;
}

/**
 * Keeps the subfolders and the footer counts of ONE pane's listing: `POST /api/browse/folders` for
 * the `BrowseRequest` the host gives it.
 *
 *   private readonly folders = new FoldersController(this);
 *   // willUpdate(): this.folders.setRequest(this.browseRequest);
 *   // render(): this.folders.folders, this.folders.counts, this.folders.status ...
 *
 * - `setRequest(request | null)`: call it on every `willUpdate`; an unchanged request does nothing.
 *   `null` (volumes not loaded, pane disabled) aborts everything and drops the data. A request for
 *   ANOTHER listing (volume, directory or filter; see `selectionContextKey`) drops the data and
 *   loads from scratch (`status: 'loading'`, `counts: null`). A request that differs only by its
 *   plan keeps the rows on screen and refetches over them (`refreshing`).
 * - Stale-while-revalidate: a refetch (after an event, a plan change or `retry`) keeps the old data
 *   and sets `refreshing`; nothing flickers. A newer request aborts the older one and an answer
 *   to anything but the newest is dropped.
 * - It refetches (debounced like `PagedItems`) after an `fs_change` that touches the directory or
 *   anything below it (folder counts are recursive), on a resync of the event stream (at once) and
 *   when an `immich_sync` task ends. The subscriptions live between connect and disconnect, and a
 *   host that connects again refetches, as events were missed meanwhile.
 * - Two failures are answers, not errors: `folder_not_found` sets `missing` (the data is dropped;
 *   the pane walks up to an ancestor) and `plan_not_found` sets `planMissing` (the data stays; the
 *   tool view builds a new plan). Anything else is `status: 'error'` with `error` and `retry()`;
 *   a failed refetch keeps the data it had.
 *
 * A request with `volume_id: null` and `dir: ''` is the volumes root: the folders are the volumes.
 * It works like any other listing.
 */
export class FoldersController implements ReactiveController {
  private readonly host: ReactiveControllerHost;
  private readonly client: Pick<ApiClient, 'post'> | undefined;
  private readonly bus: Pick<EventsClient, 'on' | 'onResync'>;
  private readonly debounceMs: number;
  private readonly maxWaitMs: number;

  private request: BrowseRequest | null = null;
  private listingKey: string | null = null;
  private planKey: string | null = null;
  private data: FoldersResponse | null = null;
  private failure: string | null = null;
  private notFound = false;
  private planGone = false;

  /** The request in flight; it is also the token that tells a current answer from a stale one. */
  private flight: AbortController | null = null;
  private connected = false;
  private unsubscribe: Array<() => void> = [];
  private debounceTimer: number | undefined;
  private debounceStart = 0;

  constructor(host: ReactiveControllerHost, options: FoldersControllerOptions = {}) {
    this.host = host;
    this.client = options.client;
    this.bus = options.events ?? sharedEvents;
    this.debounceMs = Math.max(0, options.debounceMs ?? INVALIDATE_DEBOUNCE_MS);
    this.maxWaitMs = Math.max(this.debounceMs, options.maxWaitMs ?? INVALIDATE_MAX_WAIT_MS);
    host.addController(this);
  }

  /** The subfolders of the listing (a stable empty array until the first answer). */
  get folders(): readonly FolderEntry[] {
    return this.data?.folders ?? NO_FOLDERS;
  }

  /** The footer counts; `null` until the first answer of the current listing. */
  get counts(): FooterCounts | null {
    return this.data?.counts ?? null;
  }

  get status(): FoldersStatus {
    if (this.request === null) return 'idle';
    if (this.failure !== null) return 'error';
    return this.data !== null || this.notFound ? 'ready' : 'loading';
  }

  /** The message of the failure while `status` is `error`. */
  get error(): string | null {
    return this.failure;
  }

  /** A refetch is in flight over data (or a `missing` answer) that is already shown. */
  get refreshing(): boolean {
    return this.flight !== null && this.status === 'ready';
  }

  /** The folder is not indexed and not planned: the answer was `folder_not_found`. */
  get missing(): boolean {
    return this.notFound;
  }

  /** The plan is gone from the server (`plan_not_found`); the old data is still shown. */
  get planMissing(): boolean {
    return this.planGone;
  }

  setRequest(request: BrowseRequest | null): void {
    const listingKey = request === null ? null : selectionContextKey(request);
    const planKey = request === null ? null : planKeyOf(request);
    const listingChanged = listingKey !== this.listingKey;
    const changed = listingChanged || planKey !== this.planKey;
    this.request = request;
    if (!changed) return;
    this.listingKey = listingKey;
    this.planKey = planKey;
    this.cancelDebounce();
    this.abortFlight();
    this.failure = null;
    this.planGone = false;
    if (listingChanged) {
      this.data = null;
      this.notFound = false;
    }
    this.load();
    this.host.requestUpdate();
  }

  /** Tries again after an error: the message goes at once and the rows stay while it loads. */
  retry(): void {
    if (this.request === null) return;
    this.cancelDebounce();
    this.failure = null;
    this.load();
    this.host.requestUpdate();
  }

  /** Refetches now (an event does it by itself, debounced). A no-op without a request. */
  invalidate(): void {
    this.cancelDebounce();
    this.load();
  }

  hostConnected(): void {
    if (this.connected) return;
    this.connected = true;
    this.unsubscribe = [
      this.bus.on('fs_change', this.onFsChange),
      this.bus.onResync(this.onResync),
      this.bus.on('task', this.onTask),
    ];
    this.load();
  }

  hostDisconnected(): void {
    this.connected = false;
    for (const stop of this.unsubscribe) stop();
    this.unsubscribe = [];
    this.cancelDebounce();
    this.abortFlight();
  }

  private readonly onFsChange = (event: ServerEventOf<'fs_change'>): void => {
    if (this.request !== null && fsChangeAffects(this.request, event, { subtree: true })) {
      this.invalidateSoon();
    }
  };

  private readonly onResync = (): void => {
    this.invalidate();
  };

  private readonly onTask = (event: ServerEventOf<'task'>): void => {
    if (event.task.kind === 'immich_sync' && isEnded(event.task)) this.invalidateSoon();
  };

  /** Starts the request for the current listing, replacing the one in flight. */
  private load(): void {
    const request = this.request;
    if (request === null || !this.connected) return;
    this.abortFlight();
    const flight = new AbortController();
    this.flight = flight;
    void this.run(request, flight);
    this.host.requestUpdate();
  }

  private async run(request: BrowseRequest, flight: AbortController): Promise<void> {
    try {
      const response = await fetchFolders(request, { signal: flight.signal, client: this.client });
      if (this.flight !== flight) return;
      this.flight = null;
      this.data = response;
      this.notFound = false;
      this.planGone = false;
      this.failure = null;
    } catch (error) {
      if (this.flight !== flight) return;
      this.flight = null;
      if (!isAbortError(error)) this.fail(error);
    }
    this.host.requestUpdate();
  }

  private fail(error: unknown): void {
    const code = isApiError(error) ? error.code : null;
    if (code === 'folder_not_found') {
      this.data = null;
      this.notFound = true;
      this.failure = null;
    } else if (code === 'plan_not_found') {
      this.planGone = true;
      this.failure = null;
    } else {
      this.failure = errorMessage(error);
    }
  }

  private abortFlight(): void {
    const flight = this.flight;
    this.flight = null;
    flight?.abort();
  }

  /** `invalidate`, delayed for bursts of events (a scan or an import emits many). */
  private invalidateSoon(): void {
    const now = Date.now();
    if (this.debounceTimer === undefined) this.debounceStart = now;
    else window.clearTimeout(this.debounceTimer);
    const wait = Math.min(this.debounceMs, Math.max(0, this.debounceStart + this.maxWaitMs - now));
    this.debounceTimer = window.setTimeout(() => {
      this.debounceTimer = undefined;
      this.invalidate();
    }, wait);
  }

  private cancelDebounce(): void {
    if (this.debounceTimer === undefined) return;
    window.clearTimeout(this.debounceTimer);
    this.debounceTimer = undefined;
  }
}
