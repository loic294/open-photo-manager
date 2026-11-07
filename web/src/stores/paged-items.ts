import { batch, type ReadonlySignal, signal } from '@preact/signals-core';
import type { ApiClient } from '@/api/client';
import { errorMessage, isAbortError } from '@/api/errors';
import { fetchItems, ITEMS_PAGE_SIZE } from '@/api/files';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import type { FileItem } from '@/api/generated/FileItem';
import type { ItemsResponse } from '@/api/generated/ItemsResponse';
import type { SortKey } from '@/api/generated/SortKey';
import type { SortOrder } from '@/api/generated/SortOrder';
import { type EventsClient, events as sharedEvents } from '@/api/sse';
import { fsChangeAffects } from '@/util/fs-change';

/** Pages kept in memory (least recently read go first): 40 x 200 items. */
export const MAX_CACHED_PAGES = 40;
/** Quiet time after the last matching `fs_change` before the listing is refetched. */
export const INVALIDATE_DEBOUNCE_MS = 200;
/** A steady stream of events cannot postpone the refetch for longer than this. */
export const INVALIDATE_MAX_WAIT_MS = 2000;

export interface PagedItemsOptions {
  /** Defaults to the shared `api`. */
  client?: Pick<ApiClient, 'post'>;
  /** Defaults to the shared `events`. */
  events?: Pick<EventsClient, 'on' | 'onResync'>;
  pageSize?: number;
  maxPages?: number;
  debounceMs?: number;
  maxWaitMs?: number;
}

interface CachedPage {
  items: readonly FileItem[];
  /** Shown as is, but refetched the next time something reads it. */
  stale: boolean;
}

/** Pages refetched together after an invalidation: nothing shows until all of them are in. */
interface Revalidation {
  pending: number;
  responses: Map<number, ItemsResponse>;
  failures: Map<number, unknown>;
}

interface Flight {
  controller: AbortController;
  revalidation: Revalidation | null;
}

function positiveInt(value: number | undefined, fallback: number): number {
  return value !== undefined && Number.isFinite(value) && value >= 1 ? Math.floor(value) : fallback;
}

/** Same JSON whatever the key order, so an equal request built differently is not a change. */
function canonicalKey(value: unknown): string {
  return JSON.stringify(value, (_key, item: unknown) =>
    typeof item === 'object' && item !== null && !Array.isArray(item)
      ? Object.fromEntries(Object.entries(item).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)))
      : item,
  );
}

/**
 * A listing of unknown length read by index, for a virtualised grid. It fetches the pages of
 * `ITEMS_PAGE_SIZE` items that something asks for and keeps the most recent ones.
 *
 * Reading: `get(index)` returns the cached item, or `undefined` while its page is missing, and
 * then schedules that page. It never writes a signal, so it is safe inside `render()`. Reads of
 * one animation frame are batched into one flush (a page is fetched once however many cells ask
 * for it), and a request for a page nobody reads any more is cancelled, so a fast scroll does
 * not queue hundreds of requests. Read `total`, `version`, `error` and `loading` in `render()`:
 * `version` changes whenever cached items change, which is the cue to call `get` again.
 *
 * Changing: `reset` starts a new listing (cache dropped, `total` back to `null` until the first
 * page is in). `invalidate` (also run, debounced, by a matching `fs_change`, and at once after
 * an event stream resync) keeps what is cached and visible and swaps it for fresh data in ONE
 * `version` change once every page on screen has been refetched: stale-while-revalidate, never
 * an empty frame. Other cached pages are refetched the next time they are read.
 *
 * Failing: `error` holds the message and the page is not retried by itself (a render would loop):
 * `retry()` does, and so does an invalidation. A page that failed while old data was on screen
 * leaves the old data in place.
 */
export class PagedItems {
  private readonly totalSignal = signal<number | null>(null);
  private readonly versionSignal = signal(0);
  private readonly errorSignal = signal<string | null>(null);
  private readonly loadingSignal = signal(false);

  /** Number of items in the listing; `null` until the first page of a listing arrived. */
  readonly total: ReadonlySignal<number | null> = this.totalSignal;
  /** Bumped whenever cached items change: re-render, then read the items again. */
  readonly version: ReadonlySignal<number> = this.versionSignal;
  /** Message of the last failed fetch; cleared when every failed page was fetched again. */
  readonly error: ReadonlySignal<string | null> = this.errorSignal;
  /** A page that is not cached yet is being fetched. Revalidating shown data is not loading. */
  readonly loading: ReadonlySignal<boolean> = this.loadingSignal;

  private readonly client: Pick<ApiClient, 'post'> | undefined;
  private readonly pageSize: number;
  private readonly maxPages: number;
  private readonly debounceMs: number;
  private readonly maxWaitMs: number;
  private readonly unsubscribe: Array<() => void>;

  private request: BrowseRequest | null = null;
  private sort: SortKey = 'name';
  private order: SortOrder = 'asc';
  private key: string | null = null;

  /** Insertion order is recency order: the first entry is the next to be evicted. */
  private readonly pages = new Map<number, CachedPage>();
  private readonly inflight = new Map<number, Flight>();
  private readonly failed = new Set<number>();
  private wanted = new Set<number>();
  /** Pages read since the last flush, and the pages read by the flush before: what is on screen. */
  private touched = new Set<number>();
  private visible = new Set<number>();

  private frame: number | undefined;
  private debounceTimer: number | undefined;
  private debounceStart = 0;
  private disposed = false;

  constructor(options: PagedItemsOptions = {}) {
    this.client = options.client;
    this.pageSize = positiveInt(options.pageSize, ITEMS_PAGE_SIZE);
    this.maxPages = positiveInt(options.maxPages, MAX_CACHED_PAGES);
    this.debounceMs = Math.max(0, options.debounceMs ?? INVALIDATE_DEBOUNCE_MS);
    this.maxWaitMs = Math.max(this.debounceMs, options.maxWaitMs ?? INVALIDATE_MAX_WAIT_MS);
    const bus = options.events ?? sharedEvents;
    this.unsubscribe = [
      bus.on('fs_change', (event) => {
        if (this.request !== null && fsChangeAffects(this.request, event)) this.invalidateSoon();
      }),
      bus.onResync(() => this.invalidate()),
    ];
  }

  /**
   * Shows another listing. Does nothing when the request, the sort and the order are the same as
   * before (compared by value). Otherwise the cache and pending work are dropped, `total` and
   * `error` go back to `null` and the first page is fetched at once. Returns whether it started
   * another listing.
   */
  reset(request: BrowseRequest, sort: SortKey, order: SortOrder): boolean {
    if (this.disposed) return false;
    const key = canonicalKey({
      volume_id: request.volume_id,
      dir: request.dir,
      filter: request.filter,
      plan: request.plan,
      sort,
      order,
    });
    this.request = request;
    this.sort = sort;
    this.order = order;
    if (key === this.key) return false;
    this.key = key;

    this.cancelDebounce();
    this.cancelFrame();
    this.pages.clear();
    this.failed.clear();
    this.wanted = new Set();
    this.touched = new Set();
    this.visible = new Set();
    batch(() => {
      this.abortAll();
      this.totalSignal.value = null;
      this.errorSignal.value = null;
      this.versionSignal.value += 1;
      this.startFetch(0, null);
    });
    return true;
  }

  /**
   * The cached item at `index`, else `undefined` (not loaded yet, failed, or out of range). A
   * missing or stale page is scheduled. Never writes a signal.
   */
  get(index: number): FileItem | undefined {
    const total = this.totalSignal.peek();
    if (
      this.disposed ||
      total === null ||
      !Number.isInteger(index) ||
      index < 0 ||
      index >= total
    ) {
      return undefined;
    }
    const pageNo = Math.floor(index / this.pageSize);
    if (!this.touched.has(pageNo)) {
      this.touched.add(pageNo);
      this.schedule();
    }
    const page = this.pages.get(pageNo);
    if (page === undefined) {
      this.want(pageNo);
      return undefined;
    }
    this.pages.delete(pageNo);
    this.pages.set(pageNo, page);
    if (page.stale) this.want(pageNo);
    return page.items[index - pageNo * this.pageSize];
  }

  /** Fetches again what failed and is on screen (or the first page, if it never arrived). */
  retry(): void {
    if (this.disposed || this.request === null) return;
    if (this.totalSignal.peek() === null) this.wanted.add(0);
    for (const pageNo of this.failed) {
      if (this.visible.has(pageNo) || this.touched.has(pageNo)) this.wanted.add(pageNo);
    }
    this.failed.clear();
    this.errorSignal.value = null;
    this.cancelFrame();
    this.flush();
  }

  /**
   * The data may have changed on the server: refetch what is on screen and swap it in at once.
   * Shown items stay until then. A no-op before the first `reset`.
   */
  invalidate(): void {
    if (this.disposed || this.request === null) return;
    this.cancelDebounce();
    this.wanted = new Set();
    this.failed.clear();
    for (const page of this.pages.values()) page.stale = true;
    batch(() => {
      this.abortAll();
      const total = this.totalSignal.peek();
      if (total === null) {
        this.startFetch(0, null);
        return;
      }
      const targets = [...new Set([...this.visible, ...this.touched])].filter(
        (pageNo) => pageNo === 0 || pageNo * this.pageSize < total,
      );
      if (targets.length === 0) return;
      const revalidation: Revalidation = {
        pending: targets.length,
        responses: new Map(),
        failures: new Map(),
      };
      for (const pageNo of targets) this.startFetch(pageNo, revalidation);
    });
  }

  /** Cancels everything and unsubscribes. The signals keep their last values; `get` is inert. */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.cancelDebounce();
    this.cancelFrame();
    for (const stop of this.unsubscribe) stop();
    this.unsubscribe.length = 0;
    this.abortAll();
    this.pages.clear();
    this.failed.clear();
    this.wanted.clear();
    this.touched.clear();
    this.visible.clear();
  }

  private want(pageNo: number): void {
    if (this.inflight.has(pageNo) || this.failed.has(pageNo)) return;
    this.wanted.add(pageNo);
    this.schedule();
  }

  private schedule(): void {
    if (this.frame === undefined) {
      this.frame = window.requestAnimationFrame(() => {
        this.frame = undefined;
        this.flush();
      });
    }
  }

  private cancelFrame(): void {
    if (this.frame === undefined) return;
    window.cancelAnimationFrame(this.frame);
    this.frame = undefined;
  }

  /** One animation frame of reads: remember what is on screen, drop what is not, fetch the rest. */
  private flush(): void {
    if (this.disposed) return;
    if (this.touched.size > 0) {
      this.visible = this.touched;
      this.touched = new Set();
      this.cancelSuperseded();
    }
    const wanted = this.wanted;
    this.wanted = new Set();
    batch(() => {
      for (const pageNo of wanted) {
        const cached = this.pages.get(pageNo);
        if (!this.failed.has(pageNo) && (cached === undefined || cached.stale)) {
          this.startFetch(pageNo, null);
        }
      }
    });
  }

  /** Aborts requests for pages that scrolled away (revalidations always run to the end). */
  private cancelSuperseded(): void {
    let cancelled = false;
    for (const [pageNo, flight] of this.inflight) {
      if (flight.revalidation === null && !this.visible.has(pageNo)) {
        this.inflight.delete(pageNo);
        flight.controller.abort();
        cancelled = true;
      }
    }
    if (cancelled) this.refreshLoading();
  }

  private abortAll(): void {
    const flights = [...this.inflight.values()];
    this.inflight.clear();
    for (const flight of flights) flight.controller.abort();
    this.refreshLoading();
  }

  private refreshLoading(): void {
    let loading = false;
    for (const pageNo of this.inflight.keys()) {
      if (!this.pages.has(pageNo)) {
        loading = true;
        break;
      }
    }
    this.loadingSignal.value = loading;
  }

  private startFetch(pageNo: number, revalidation: Revalidation | null): void {
    const request = this.request;
    if (request === null || this.disposed || this.inflight.has(pageNo)) return;
    const flight: Flight = { controller: new AbortController(), revalidation };
    this.inflight.set(pageNo, flight);
    this.refreshLoading();
    fetchItems(request, this.sort, this.order, pageNo * this.pageSize, this.pageSize, {
      signal: flight.controller.signal,
      client: this.client,
    }).then(
      (response) => this.settle(pageNo, flight, response, undefined),
      (failure: unknown) => this.settle(pageNo, flight, undefined, failure),
    );
  }

  private settle(
    pageNo: number,
    flight: Flight,
    response: ItemsResponse | undefined,
    failure: unknown,
  ): void {
    // Aborted, replaced or disposed meanwhile: whatever arrived is about another listing or moment.
    if (this.inflight.get(pageNo) !== flight) return;
    this.inflight.delete(pageNo);
    if (response === undefined && isAbortError(failure)) {
      this.refreshLoading();
      return;
    }
    const { revalidation } = flight;
    if (revalidation !== null) {
      if (response === undefined) revalidation.failures.set(pageNo, failure);
      else revalidation.responses.set(pageNo, response);
      revalidation.pending -= 1;
      if (revalidation.pending === 0) this.commit(revalidation);
      else this.refreshLoading();
      return;
    }
    batch(() => {
      if (response === undefined) {
        this.failed.add(pageNo);
        this.errorSignal.value = errorMessage(failure, 'Could not load the files');
      } else {
        this.failed.delete(pageNo);
        this.store(pageNo, response);
        if (this.failed.size === 0) this.errorSignal.value = null;
        this.versionSignal.value += 1;
      }
      this.refreshLoading();
    });
  }

  /** Applies a finished revalidation in one go: one `version` change, whatever the page count. */
  private commit(revalidation: Revalidation): void {
    batch(() => {
      for (const [pageNo, response] of revalidation.responses) this.store(pageNo, response);
      if (revalidation.failures.size > 0) {
        for (const pageNo of revalidation.failures.keys()) this.failed.add(pageNo);
        const [first] = revalidation.failures.values();
        this.errorSignal.value = errorMessage(first, 'Could not load the files');
      } else if (this.failed.size === 0) {
        this.errorSignal.value = null;
      }
      if (revalidation.responses.size > 0) this.versionSignal.value += 1;
      this.refreshLoading();
    });
  }

  /** Caches a page as the most recent one. A different `total` makes every other page suspect. */
  private store(pageNo: number, response: ItemsResponse): void {
    const previous = this.totalSignal.peek();
    const { total } = response;
    if (previous !== total) {
      this.totalSignal.value = total;
      if (previous !== null) {
        const pageCount = Math.ceil(total / this.pageSize);
        for (const [cachedNo, page] of [...this.pages]) {
          if (cachedNo !== 0 && cachedNo >= pageCount) this.pages.delete(cachedNo);
          else page.stale = true;
        }
      }
    }
    if (pageNo !== 0 && pageNo * this.pageSize >= total) return;
    this.pages.delete(pageNo);
    this.pages.set(pageNo, { items: response.items, stale: false });
    while (this.pages.size > this.maxPages) {
      const oldest = this.pages.keys().next();
      if (oldest.done) break;
      this.pages.delete(oldest.value);
    }
  }

  /** Debounced `invalidate` for bursts of `fs_change` events (a scan or an import emits many). */
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
