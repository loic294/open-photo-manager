import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { isAbortError } from '@/api/errors';
import { FakeImage } from '@/test/fake-image';
import { loadImage } from '@/util/load-image';

function watch(promise: Promise<void>) {
  const state = { settled: false, error: undefined as unknown };
  promise.then(
    () => {
      state.settled = true;
    },
    (error: unknown) => {
      state.settled = true;
      state.error = error;
    },
  );
  return state;
}

const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

function onlyImage(): FakeImage {
  expect(FakeImage.instances).toHaveLength(1);
  return FakeImage.instances[0] as FakeImage;
}

beforeEach(() => {
  FakeImage.instances = [];
  vi.stubGlobal('Image', FakeImage);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('loadImage', () => {
  it('requests the url on a detached image and waits for the load', async () => {
    const load = loadImage('/api/files/1/preview');
    const state = watch(load.done);

    expect(onlyImage().src).toBe('/api/files/1/preview');
    await flush();
    expect(state.settled).toBe(false);

    onlyImage().finish();
    await load.done;
    expect(state.error).toBeUndefined();
  });

  it('waits for the decode as well', async () => {
    const load = loadImage('/a.jpg');
    const state = watch(load.done);
    let finishDecode: () => void = () => undefined;
    const image = onlyImage();
    image.decode = () =>
      new Promise<void>((resolve) => {
        finishDecode = resolve;
      });

    image.finish();
    await flush();
    expect(state.settled).toBe(false);

    finishDecode();
    await load.done;
    expect(state.error).toBeUndefined();
  });

  it('treats a failed decode as a successful load', async () => {
    const load = loadImage('/a.jpg');
    const image = onlyImage();
    image.decode = () => Promise.reject(new DOMException('broken', 'EncodingError'));

    image.finish();
    await expect(load.done).resolves.toBeUndefined();
  });

  it('settles on the load alone in a browser without decode()', async () => {
    const load = loadImage('/a.jpg');
    const image = onlyImage();
    image.decode = undefined;

    image.finish();
    await expect(load.done).resolves.toBeUndefined();
  });

  it('rejects when the image cannot be loaded', async () => {
    const load = loadImage('/missing.jpg');
    onlyImage().fail();
    const error = await load.done.catch((reason: unknown) => reason);
    expect(error).toBeInstanceOf(Error);
    expect(isAbortError(error)).toBe(false);
    expect((error as Error).message).toContain('/missing.jpg');
  });

  it('cancels: drops the request, rejects with an AbortError and ignores late events', async () => {
    const load = loadImage('/a.jpg');
    const image = onlyImage();

    load.cancel();

    expect(image.src).toBe('');
    expect(FakeImage.requested('/a.jpg')).toHaveLength(0);
    expect(image.onload).toBeNull();
    expect(image.onerror).toBeNull();
    await expect(load.done).rejects.toSatisfy(isAbortError);
  });

  it('is safe to cancel twice, and after it settled', async () => {
    const first = loadImage('/a.jpg');
    first.cancel();
    first.cancel();
    await expect(first.done).rejects.toSatisfy(isAbortError);

    const second = loadImage('/b.jpg');
    FakeImage.instances[1]?.finish();
    await second.done;
    second.cancel();
    await expect(second.done).resolves.toBeUndefined();
  });

  it('does not report a cancelled load nobody awaits as an unhandled rejection', async () => {
    loadImage('/a.jpg').cancel();
    await flush();
    // Vitest fails the run on an unhandled rejection: reaching this line is the assertion.
    expect(FakeImage.instances).toHaveLength(1);
  });
});
