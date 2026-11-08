/**
 * A stand-in for `Image` whose requests the test settles by hand:
 * `vi.stubGlobal('Image', FakeImage)` and `FakeImage.instances.length = 0` between tests. Anything
 * that loads an image off-screen (`loadImage`) goes through it, so a test can tell what was
 * requested, what was abandoned, and decide when each request finishes.
 */
export class FakeImage {
  static instances: FakeImage[] = [];

  /** The instances that currently hold a request for `url` (not abandoned, `src` still set). */
  static requested(url: string): FakeImage[] {
    return FakeImage.instances.filter((image) => image.src === url);
  }

  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  src = '';
  /** Replace it to control decoding; `undefined` is a browser without `decode()`. */
  decode: (() => Promise<void>) | undefined = () => Promise.resolve();

  constructor() {
    FakeImage.instances.push(this);
  }

  removeAttribute(name: string): void {
    if (name === 'src') this.src = '';
  }

  /** The browser finished fetching the image. */
  finish(): void {
    this.onload?.();
  }

  /** The request failed. */
  fail(): void {
    this.onerror?.();
  }
}
