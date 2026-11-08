/** An image request that can be abandoned; see `loadImage`. */
export interface ImageLoad {
  /**
   * Resolves once the image is loaded and, where the browser can, decoded (so showing it does not
   * stall painting). Rejects with an `Error` when it fails to load and with an `AbortError`
   * (`isAbortError`) after `cancel()`.
   */
  readonly done: Promise<void>;
  /** Abandons the request, so the browser aborts it, and drops the image. Safe to call twice. */
  cancel(): void;
}

/**
 * Loads `url` into a detached `Image`. The browser keeps what it fetched, so an `<img>` given the
 * same `src` afterwards shows it at once: that is how the viewer swaps a thumbnail for the preview
 * and preloads a neighbour. Hold on to the returned object for as long as the image should stay
 * warm.
 */
export function loadImage(url: string): ImageLoad {
  const image = new Image();
  let abandon: (reason: unknown) => void = () => undefined;

  const done = new Promise<void>((resolve, reject) => {
    abandon = reject;
    image.onerror = () => reject(new Error(`The image ${url} could not be loaded`));
    image.onload = () => {
      // A failed decode is not a failed load: the browser still paints what it has.
      const decoded = typeof image.decode === 'function' ? image.decode() : Promise.resolve();
      decoded.then(
        () => resolve(),
        () => resolve(),
      );
    };
    image.src = url;
  });
  // Whoever cancels does not await `done`: a rejection nobody reads is not an error.
  done.catch(() => undefined);

  return {
    done,
    cancel() {
      image.onload = null;
      image.onerror = null;
      image.removeAttribute('src');
      abandon(new DOMException('The image load was cancelled', 'AbortError'));
    },
  };
}
