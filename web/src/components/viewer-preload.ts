import { previewUrl, thumbUrl } from '@/api/files';
import type { FileItem } from '@/api/generated/FileItem';
import { hasThumbnail } from '@/components/apm-file-card';
import { shouldPreloadPreview } from '@/components/viewer-nav';
import { type ImageLoad, loadImage } from '@/util/load-image';

/** Finished preloads kept in the browser's cache by holding their image: two files' worth. */
export const WARM_MAX = 4;

/**
 * The row of file `id`: the one the viewer already has, else the file's detail from the server.
 * It rejects when `signal` aborts.
 */
export type ItemResolver = (id: FileItem['id'], signal: AbortSignal) => Promise<FileItem>;

/**
 * Warms the browser's cache with the file the user is about to step to, so the viewer shows it at
 * once. The NAS is slow, so it is deliberately cheap and quick to give up:
 *
 * - the neighbour's large thumbnail first (images and videos: a video's poster), then its
 *   `/preview`, and only for an image of at most `PRELOAD_MAX_BYTES`: the preview of a video or
 *   a big photo is the whole original file, so it is fetched when the user gets there;
 * - one neighbour at a time: `start()` and `cancel()` abandon what is in flight, and the viewer
 *   cancels as soon as the user moves;
 * - the item is resolved first (`ItemResolver`) because the thumbnail URL carries its `mtime`.
 *
 * Failures are ignored: the viewer asks again when the user gets there.
 */
export class ViewerPreloader {
  private readonly resolve: ItemResolver;
  private controller: AbortController | undefined;
  private inFlight: ImageLoad | undefined;
  private warm: ImageLoad[] = [];

  constructor(resolve: ItemResolver) {
    this.resolve = resolve;
  }

  /** Preloads file `id`, abandoning the preload in progress. */
  start(id: FileItem['id']): void {
    this.cancel();
    const controller = new AbortController();
    this.controller = controller;
    void this.preload(id, controller.signal);
  }

  /** Abandons what is in flight. What has finished stays warm. */
  cancel(): void {
    this.controller?.abort();
    this.controller = undefined;
    this.inFlight?.cancel();
    this.inFlight = undefined;
  }

  /** Abandons everything, finished preloads included. */
  clear(): void {
    this.cancel();
    for (const load of this.warm) load.cancel();
    this.warm = [];
  }

  private async preload(id: FileItem['id'], signal: AbortSignal): Promise<void> {
    let item: FileItem;
    try {
      item = await this.resolve(id, signal);
    } catch {
      return;
    }
    if (signal.aborted) return;
    if (hasThumbnail(item.kind)) await this.warmUp(thumbUrl(item, 'lg'), signal);
    if (!signal.aborted && shouldPreloadPreview(item))
      await this.warmUp(previewUrl(item.id), signal);
  }

  /** Never rejects: a thumbnail that cannot be fetched does not stop the preview. */
  private async warmUp(url: string, signal: AbortSignal): Promise<void> {
    const load = loadImage(url);
    this.inFlight = load;
    try {
      await load.done;
    } catch {
      return;
    } finally {
      if (this.inFlight === load) this.inFlight = undefined;
    }
    if (signal.aborted) {
      load.cancel();
      return;
    }
    this.warm.push(load);
    while (this.warm.length > WARM_MAX) this.warm.shift()?.cancel();
  }
}
