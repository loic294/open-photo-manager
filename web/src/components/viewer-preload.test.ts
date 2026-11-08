import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { previewUrl, thumbUrl } from '@/api/files';
import type { FileItem } from '@/api/generated/FileItem';
import { PRELOAD_MAX_BYTES } from '@/components/viewer-nav';
import { type ItemResolver, ViewerPreloader, WARM_MAX } from '@/components/viewer-preload';
import { FakeImage } from '@/test/fake-image';
import { fileItem } from '@/test/file-item';

beforeEach(() => {
  FakeImage.instances.length = 0;
  vi.stubGlobal('Image', FakeImage);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

const photo = (id: number, overrides: Partial<FileItem> = {}) =>
  fileItem({ id, kind: 'image', size: 1_000_000, ...overrides });

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

async function settle(): Promise<void> {
  for (let round = 0; round < 12; round += 1) await Promise.resolve();
}

/** The urls that are being or have been fetched and not abandoned since. */
const requested = () => FakeImage.instances.filter((image) => image.src !== '').map((i) => i.src);

async function finish(url: string): Promise<void> {
  const [request] = FakeImage.requested(url);
  if (!request) throw new Error(`Nothing requested ${url}`);
  request.finish();
  await settle();
}

function preloaderOf(...items: FileItem[]) {
  const signals: AbortSignal[] = [];
  const resolve = vi.fn<ItemResolver>(async (id, signal) => {
    signals.push(signal);
    const item = items.find((candidate) => candidate.id === id);
    if (!item) throw new Error(`The file ${id} does not exist`);
    return item;
  });
  return { preloader: new ViewerPreloader(resolve), resolve, signals };
}

describe('what is preloaded', () => {
  it('loads the large thumbnail of an image, then its preview', async () => {
    const item = photo(5);
    const { preloader, resolve } = preloaderOf(item);

    preloader.start(5);
    await settle();

    expect(resolve).toHaveBeenCalledWith(5, expect.any(AbortSignal));
    expect(requested()).toEqual([thumbUrl(item, 'lg')]);

    await finish(thumbUrl(item, 'lg'));
    expect(requested()).toEqual([thumbUrl(item, 'lg'), previewUrl(5)]);
  });

  it('loads the poster of a video and never the video', async () => {
    const item = photo(6, { kind: 'video', size: 10_000_000 });
    const { preloader } = preloaderOf(item);

    preloader.start(6);
    await settle();
    await finish(thumbUrl(item, 'lg'));

    expect(requested()).toEqual([thumbUrl(item, 'lg')]);
  });

  it.each(['audio', 'sidecar', 'other'] as const)('loads nothing of a %s file', async (kind) => {
    const { preloader } = preloaderOf(photo(7, { kind, size: 10 }));

    preloader.start(7);
    await settle();

    expect(FakeImage.instances).toEqual([]);
  });

  it('preloads the preview of an image at the size cap but not above it', async () => {
    const atCap = photo(8, { size: PRELOAD_MAX_BYTES });
    const above = photo(9, { size: PRELOAD_MAX_BYTES + 1 });
    const { preloader } = preloaderOf(atCap, above);

    preloader.start(8);
    await settle();
    await finish(thumbUrl(atCap, 'lg'));
    expect(requested()).toContain(previewUrl(8));

    preloader.start(9);
    await settle();
    await finish(thumbUrl(above, 'lg'));
    expect(requested()).not.toContain(previewUrl(9));
  });

  it('carries on to the preview when the thumbnail cannot be fetched', async () => {
    const item = photo(5);
    const { preloader } = preloaderOf(item);

    preloader.start(5);
    await settle();
    FakeImage.requested(thumbUrl(item, 'lg'))[0]?.fail();
    await settle();

    expect(requested()).toContain(previewUrl(5));
  });

  it('ignores a file it cannot resolve', async () => {
    const { preloader } = preloaderOf();

    preloader.start(404);
    await settle();

    expect(FakeImage.instances).toEqual([]);
  });
});

describe('giving up', () => {
  it('aborts the lookup and starts nothing once cancelled', async () => {
    const lookup = deferred<FileItem>();
    let seen: AbortSignal | undefined;
    const preloader = new ViewerPreloader((_id, signal) => {
      seen = signal;
      return lookup.promise;
    });

    preloader.start(5);
    preloader.cancel();
    lookup.resolve(photo(5));
    await settle();

    expect(seen?.aborted).toBe(true);
    expect(FakeImage.instances).toEqual([]);
  });

  it('abandons the request in flight but keeps what has finished', async () => {
    const item = photo(5);
    const { preloader } = preloaderOf(item);
    preloader.start(5);
    await settle();
    await finish(thumbUrl(item, 'lg'));
    const [preview] = FakeImage.requested(previewUrl(5));
    expect(preview).toBeDefined();

    preloader.cancel();

    expect(preview?.src).toBe('');
    expect(requested()).toEqual([thumbUrl(item, 'lg')]);
  });

  it('abandons the file in progress when told to start another', async () => {
    const first = photo(1);
    const second = photo(2);
    const { preloader, signals } = preloaderOf(first, second);
    preloader.start(1);
    await settle();

    preloader.start(2);
    await settle();

    expect(signals[0]?.aborted).toBe(true);
    expect(signals[1]?.aborted).toBe(false);
    expect(requested()).toEqual([thumbUrl(second, 'lg')]);
  });

  it('does not keep a request that finished after it was cancelled', async () => {
    const item = photo(5);
    const { preloader } = preloaderOf(item);
    preloader.start(5);
    await settle();
    const [thumb] = FakeImage.requested(thumbUrl(item, 'lg'));
    const decoding = deferred<void>();
    if (thumb) thumb.decode = () => decoding.promise;
    thumb?.finish();
    await settle();

    preloader.cancel();
    decoding.resolve();
    await settle();

    expect(requested()).toEqual([]);
  });

  it('clears everything, finished requests included', async () => {
    const item = photo(5);
    const { preloader } = preloaderOf(item);
    preloader.start(5);
    await settle();
    await finish(thumbUrl(item, 'lg'));
    await finish(previewUrl(5));
    expect(requested()).toHaveLength(2);

    preloader.clear();

    expect(requested()).toEqual([]);
  });
});

describe('the warm ring', () => {
  it('keeps at most WARM_MAX finished requests, dropping the oldest', async () => {
    const [one, two, three] = [photo(1), photo(2), photo(3)];
    const { preloader } = preloaderOf(one, two, three);

    for (const item of [one, two, three]) {
      preloader.start(item.id);
      await settle();
      await finish(thumbUrl(item, 'lg'));
      await finish(previewUrl(item.id));
    }

    expect(WARM_MAX).toBe(4);
    expect(requested()).toEqual([
      thumbUrl(two, 'lg'),
      previewUrl(2),
      thumbUrl(three, 'lg'),
      previewUrl(3),
    ]);
  });
});
