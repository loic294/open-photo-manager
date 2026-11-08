import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import '@/components/apm-viewer-stage';
import { contentUrl, previewUrl, thumbUrl } from '@/api/files';
import type { FileItem } from '@/api/generated/FileItem';
import type { ApmViewerStage } from '@/components/apm-viewer-stage';
import { FakeImage } from '@/test/fake-image';
import { fileItem } from '@/test/file-item';
import { mount } from '@/test/helpers';
import { formatFileSize } from '@/util/format';

const photo = (overrides: Partial<FileItem> = {}) => fileItem({ kind: 'image', ...overrides });
const clip = (overrides: Partial<FileItem> = {}) =>
  fileItem({ id: 9, kind: 'video', name: 'CLIP_0009.mp4', duration_ms: 12_000, ...overrides });
const song = () => fileItem({ id: 11, kind: 'audio', name: 'voice.m4a', size: 123_456 });

beforeEach(() => {
  FakeImage.instances.length = 0;
  vi.stubGlobal('Image', FakeImage);
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

interface Opened {
  stage: ApmViewerStage;
  settled: FileItem['id'][];
}

/** Mounts a stage with its `stage-settled` listener in place before the first render. */
async function open(
  item: FileItem | undefined,
  props: Partial<ApmViewerStage> = {},
): Promise<Opened> {
  const stage = document.createElement('apm-viewer-stage');
  const settled: FileItem['id'][] = [];
  stage.addEventListener('stage-settled', (event) => settled.push(event.detail.id));
  await mount(stage, { item, ...props });
  return { stage, settled };
}

async function flush(stage: ApmViewerStage): Promise<void> {
  for (let round = 0; round < 3; round += 1) {
    await Promise.resolve();
    await stage.updateComplete;
  }
}

async function update(stage: ApmViewerStage, props: Partial<ApmViewerStage>): Promise<void> {
  Object.assign(stage, props);
  await flush(stage);
}

/** The browser finishes the off-screen request for `url`. */
async function finish(stage: ApmViewerStage, url: string): Promise<void> {
  const [request] = FakeImage.requested(url);
  if (!request) throw new Error(`Nothing requested ${url}`);
  request.finish();
  await flush(stage);
}

async function fail(stage: ApmViewerStage, url: string): Promise<void> {
  const [request] = FakeImage.requested(url);
  if (!request) throw new Error(`Nothing requested ${url}`);
  request.fail();
  await flush(stage);
}

function part<T extends Element = HTMLElement>(root: ParentNode, selector: string): T {
  const found = root.querySelector<T>(selector);
  if (!found) throw new Error(`No element matches "${selector}"`);
  return found;
}

const img = (stage: ApmViewerStage) => part<HTMLImageElement>(stage, 'img');
const video = (stage: ApmViewerStage) => part<HTMLVideoElement>(stage, 'video');
const audio = (stage: ApmViewerStage) => part<HTMLAudioElement>(stage, 'audio');
const scroller = (stage: ApmViewerStage) => part(stage, '[data-stage-scroller]');
const hasSpinner = (stage: ApmViewerStage) => stage.querySelector('[data-stage-spinner]') !== null;
const alertOf = (stage: ApmViewerStage) => part(stage, '[data-stage-error]');
const text = (element: Element) => element.textContent?.replace(/\s+/g, ' ').trim();

function tellSize(
  element: HTMLElement,
  size: { scrollWidth: number; clientWidth: number; scrollHeight: number; clientHeight: number },
): void {
  for (const [key, value] of Object.entries(size)) {
    Object.defineProperty(element, key, { value, configurable: true });
  }
}

describe('the host', () => {
  it('fills a flex parent on a dark ground', async () => {
    const { stage } = await open(photo());
    for (const name of ['relative', 'min-h-0', 'min-w-0', 'flex-1', 'bg-neutral']) {
      expect(stage.classList.contains(name)).toBe(true);
    }
  });
});

describe('an image', () => {
  it('shows the large thumbnail dimmed with a spinner, then the preview', async () => {
    const item = photo();
    const { stage, settled } = await open(item);

    expect(img(stage).getAttribute('src')).toBe(thumbUrl(item, 'lg'));
    expect(img(stage).alt).toBe(item.name);
    expect(img(stage).classList.contains('opacity-40')).toBe(true);
    expect(img(stage).classList.contains('object-contain')).toBe(true);
    expect(hasSpinner(stage)).toBe(true);
    expect(stage.getAttribute('aria-busy')).toBe('true');
    expect(FakeImage.requested(previewUrl(item.id))).toHaveLength(1);
    expect(settled).toEqual([]);

    await finish(stage, previewUrl(item.id));

    expect(img(stage).getAttribute('src')).toBe(previewUrl(item.id));
    expect(img(stage).classList.contains('opacity-40')).toBe(false);
    expect(img(stage).classList.contains('cursor-zoom-in')).toBe(true);
    expect(hasSpinner(stage)).toBe(false);
    expect(stage.getAttribute('aria-busy')).toBe('false');
    expect(settled).toEqual([item.id]);
  });

  it('never draws the thumbnail or the preview of anything but an image off-screen', async () => {
    await open(clip());
    await open(song());
    await open(fileItem({ kind: 'sidecar', name: 'IMG_0007.xmp' }));
    expect(FakeImage.instances).toEqual([]);
  });

  it('hides a thumbnail that failed to load until the preview comes', async () => {
    const item = photo();
    const { stage } = await open(item);

    img(stage).dispatchEvent(new Event('error'));
    await flush(stage);
    expect(img(stage).classList.contains('invisible')).toBe(true);
    expect(hasSpinner(stage)).toBe(true);

    await finish(stage, previewUrl(item.id));
    expect(img(stage).classList.contains('invisible')).toBe(false);
    expect(img(stage).getAttribute('src')).toBe(previewUrl(item.id));
  });

  it('offers Retry and Download when the preview cannot be loaded', async () => {
    const item = photo();
    const { stage, settled } = await open(item);

    await fail(stage, previewUrl(item.id));

    expect(text(alertOf(stage))).toContain('This image could not be loaded');
    expect(hasSpinner(stage)).toBe(false);
    expect(stage.getAttribute('aria-busy')).toBe('false');
    expect(settled).toEqual([item.id]);
    const download = part<HTMLAnchorElement>(alertOf(stage), 'a[download]');
    expect(download.getAttribute('href')).toBe(contentUrl(item.id, { download: true }));
  });

  it('retries from the start and settles again when it works', async () => {
    const item = photo();
    const { stage, settled } = await open(item);
    await fail(stage, previewUrl(item.id));
    const before = FakeImage.instances.length;

    part<HTMLButtonElement>(alertOf(stage), 'button').click();
    await flush(stage);

    expect(stage.querySelector('[data-stage-error]')).toBeNull();
    expect(hasSpinner(stage)).toBe(true);
    expect(img(stage).getAttribute('src')).toBe(thumbUrl(item, 'lg'));
    expect(FakeImage.instances).toHaveLength(before + 1);

    const retry = FakeImage.instances.at(-1);
    expect(retry?.src).toBe(previewUrl(item.id));
    retry?.finish();
    await flush(stage);

    expect(img(stage).getAttribute('src')).toBe(previewUrl(item.id));
    expect(settled).toEqual([item.id, item.id]);
  });

  it('shows the failure when the browser cannot display the preview it downloaded', async () => {
    const item = photo();
    const { stage } = await open(item);
    await finish(stage, previewUrl(item.id));

    img(stage).dispatchEvent(new Event('error'));
    await flush(stage);

    expect(text(alertOf(stage))).toContain('This image could not be loaded');
  });

  it('abandons the preview of the file it leaves and starts the next one', async () => {
    const first = photo({ id: 1 });
    const second = photo({ id: 2, name: 'IMG_0002.jpg' });
    const { stage, settled } = await open(first);
    const [abandoned] = FakeImage.requested(previewUrl(first.id));

    await update(stage, { item: second });

    expect(abandoned?.src).toBe('');
    expect(FakeImage.requested(previewUrl(second.id))).toHaveLength(1);
    expect(img(stage).getAttribute('src')).toBe(thumbUrl(second, 'lg'));
    expect(img(stage).alt).toBe(second.name);
    expect(img(stage).classList.contains('opacity-40')).toBe(true);

    abandoned?.finish();
    await flush(stage);
    expect(settled).toEqual([]);

    await finish(stage, previewUrl(second.id));
    expect(settled).toEqual([second.id]);
  });

  it('keeps the previous picture, dimmed, while the next file is not known yet', async () => {
    const item = photo();
    const { stage, settled } = await open(item);
    await finish(stage, previewUrl(item.id));

    await update(stage, { item: undefined });

    expect(img(stage).getAttribute('src')).toBe(previewUrl(item.id));
    expect(img(stage).alt).toBe('');
    expect(img(stage).classList.contains('opacity-40')).toBe(true);
    expect(hasSpinner(stage)).toBe(true);
    expect(stage.getAttribute('aria-busy')).toBe('true');

    const next = photo({ id: 8, name: 'IMG_0008.jpg' });
    await update(stage, { item: next });
    await finish(stage, previewUrl(next.id));
    expect(settled).toEqual([item.id, next.id]);
  });

  it('restarts nothing when the detail replaces the row of the same file', async () => {
    const item = photo();
    const { stage, settled } = await open(item);

    await update(stage, { item: { ...item, camera: 'Nikon Z9' } });
    expect(FakeImage.instances).toHaveLength(1);
    expect(hasSpinner(stage)).toBe(true);

    await finish(stage, previewUrl(item.id));
    await update(stage, { item: { ...item, camera: 'Sony A7' } });

    expect(FakeImage.instances).toHaveLength(1);
    expect(img(stage).getAttribute('src')).toBe(previewUrl(item.id));
    expect(hasSpinner(stage)).toBe(false);
    expect(settled).toEqual([item.id]);
  });
});

describe('zoom', () => {
  it('applies the zoom once the preview is shown, and keeps asking until then', async () => {
    const item = photo();
    const { stage } = await open(item, { zoomed: true });

    expect(scroller(stage).classList.contains('overflow-hidden')).toBe(true);
    expect(img(stage).classList.contains('object-contain')).toBe(true);

    await finish(stage, previewUrl(item.id));

    expect(scroller(stage).classList.contains('overflow-auto')).toBe(true);
    for (const name of [
      'm-auto',
      'h-auto',
      'w-auto',
      'max-w-none',
      'shrink-0',
      'cursor-zoom-out',
    ]) {
      expect(img(stage).classList.contains(name)).toBe(true);
    }
    expect(img(stage).classList.contains('object-contain')).toBe(false);
  });

  it('starts the zoomed picture centred and centres it again once it has loaded', async () => {
    const item = photo();
    const { stage } = await open(item);
    await finish(stage, previewUrl(item.id));
    tellSize(scroller(stage), {
      scrollWidth: 3000,
      clientWidth: 1000,
      scrollHeight: 2000,
      clientHeight: 500,
    });

    await update(stage, { zoomed: true });
    expect(scroller(stage).scrollLeft).toBe(1000);
    expect(scroller(stage).scrollTop).toBe(750);

    tellSize(scroller(stage), {
      scrollWidth: 4000,
      clientWidth: 1000,
      scrollHeight: 2000,
      clientHeight: 500,
    });
    img(stage).dispatchEvent(new Event('load'));
    expect(scroller(stage).scrollLeft).toBe(1500);
  });

  it('does not scroll a picture that fits', async () => {
    const item = photo();
    const { stage } = await open(item);
    await finish(stage, previewUrl(item.id));
    tellSize(scroller(stage), {
      scrollWidth: 800,
      clientWidth: 1000,
      scrollHeight: 400,
      clientHeight: 500,
    });

    await update(stage, { zoomed: true });

    expect(scroller(stage).scrollLeft).toBe(0);
    expect(scroller(stage).scrollTop).toBe(0);
  });

  it('asks for the switch on a double click, once the picture is shown', async () => {
    const item = photo();
    const { stage } = await open(item);
    const toggles: Event[] = [];
    stage.addEventListener('zoom-toggle', (event) => toggles.push(event));

    img(stage).dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    expect(toggles).toHaveLength(0);

    await finish(stage, previewUrl(item.id));
    img(stage).dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));

    expect(toggles).toHaveLength(1);
    expect(toggles[0]?.bubbles).toBe(false);
  });
});

describe('a video', () => {
  it('renders the player with controls, the thumbnail as its poster and no autoplay', async () => {
    const item = clip();
    const { stage, settled } = await open(item);
    const player = video(stage);

    expect(player.getAttribute('src')).toBe(previewUrl(item.id));
    expect(player.getAttribute('poster')).toBe(thumbUrl(item, 'lg'));
    expect(player.hasAttribute('controls')).toBe(true);
    expect(player.hasAttribute('playsinline')).toBe(true);
    expect(player.hasAttribute('autoplay')).toBe(false);
    expect(player.getAttribute('preload')).toBe('metadata');
    expect(hasSpinner(stage)).toBe(true);
    expect(settled).toEqual([]);
  });

  it('is shown once its metadata is in', async () => {
    const item = clip();
    const { stage, settled } = await open(item);

    video(stage).dispatchEvent(new Event('loadedmetadata'));
    await flush(stage);

    expect(hasSpinner(stage)).toBe(false);
    expect(stage.getAttribute('aria-busy')).toBe('false');
    expect(settled).toEqual([item.id]);
  });

  it('falls back to the poster, with the links to the original, when it cannot play', async () => {
    const item = clip();
    const { stage, settled } = await open(item);

    video(stage).dispatchEvent(new Event('error'));
    await flush(stage);

    expect(stage.querySelector('video')).toBeNull();
    expect(img(stage).getAttribute('src')).toBe(thumbUrl(item, 'lg'));
    expect(text(alertOf(stage))).toContain("Your browser can't play this video");
    expect(alertOf(stage).classList.contains('alert-warning')).toBe(true);
    const original = part<HTMLAnchorElement>(alertOf(stage), 'a[target="_blank"]');
    expect(original.getAttribute('href')).toBe(contentUrl(item.id));
    expect(original.getAttribute('rel')).toContain('noopener');
    const download = part<HTMLAnchorElement>(alertOf(stage), 'a[download]');
    expect(download.getAttribute('href')).toBe(contentUrl(item.id, { download: true }));
    expect(hasSpinner(stage)).toBe(false);
    expect(settled).toEqual([item.id]);
  });

  it('hides a poster that is not there either', async () => {
    const { stage } = await open(clip());
    video(stage).dispatchEvent(new Event('error'));
    await flush(stage);

    img(stage).dispatchEvent(new Event('error'));
    await flush(stage);

    expect(img(stage).classList.contains('invisible')).toBe(true);
  });

  it('stops and releases the player of the file it leaves', async () => {
    const pause = vi.spyOn(HTMLMediaElement.prototype, 'pause');
    const load = vi.spyOn(HTMLMediaElement.prototype, 'load');
    const first = clip();
    const { stage } = await open(first);
    const old = video(stage);

    const second = clip({ id: 10, name: 'CLIP_0010.mp4' });
    await update(stage, { item: second });

    expect(pause.mock.contexts).toContain(old);
    expect(load.mock.contexts).toContain(old);
    expect(old.hasAttribute('src')).toBe(false);
    expect(video(stage)).not.toBe(old);
    expect(video(stage).getAttribute('src')).toBe(previewUrl(second.id));
    expect(hasSpinner(stage)).toBe(true);
  });

  it('releases the player when the viewer closes, and ignores it from then on', async () => {
    const pause = vi.spyOn(HTMLMediaElement.prototype, 'pause');
    const { stage, settled } = await open(clip());
    const player = video(stage);

    stage.release();
    player.dispatchEvent(new Event('loadedmetadata'));
    player.dispatchEvent(new Event('error'));
    await flush(stage);

    expect(pause.mock.contexts).toContain(player);
    expect(player.hasAttribute('src')).toBe(false);
    expect(stage.querySelector('[data-stage-error]')).toBeNull();
    expect(settled).toEqual([]);
  });
});

describe('an audio file', () => {
  it('renders a large icon and a player, and is shown at once', async () => {
    const item = song();
    const { stage, settled } = await open(item);

    expect(part(stage, 'apm-icon[name="music"]')).toBeTruthy();
    expect(audio(stage).getAttribute('src')).toBe(previewUrl(item.id));
    expect(audio(stage).hasAttribute('controls')).toBe(true);
    expect(audio(stage).hasAttribute('autoplay')).toBe(false);
    expect(hasSpinner(stage)).toBe(false);
    expect(settled).toEqual([item.id]);
  });

  it('warns when the browser cannot play it', async () => {
    const item = song();
    const { stage } = await open(item);

    audio(stage).dispatchEvent(new Event('error'));
    await flush(stage);

    expect(text(alertOf(stage))).toContain("Your browser can't play this audio file");
    expect(part<HTMLAnchorElement>(alertOf(stage), 'a[target="_blank"]').getAttribute('href')).toBe(
      contentUrl(item.id),
    );
  });

  it('releases the player of the file it leaves', async () => {
    const pause = vi.spyOn(HTMLMediaElement.prototype, 'pause');
    const { stage } = await open(song());
    const old = audio(stage);

    await update(stage, { item: photo() });

    expect(pause.mock.contexts).toContain(old);
    expect(old.hasAttribute('src')).toBe(false);
  });
});

describe('a file without a preview', () => {
  it.each([
    ['sidecar', 'file'],
    ['other', 'file'],
  ] as const)('shows what it is and offers the download (%s)', async (kind, icon) => {
    const item = fileItem({ id: 12, kind, name: 'IMG_0007.xmp', size: 2048 });
    const { stage, settled } = await open(item);

    expect(text(stage)).toContain('No preview for this kind of file');
    expect(text(stage)).toContain(`IMG_0007.xmp · ${formatFileSize(2048)}`);
    expect(part(stage, `apm-icon[name="${icon}"]`)).toBeTruthy();
    const download = part<HTMLAnchorElement>(stage, 'a[download]');
    expect(download.getAttribute('href')).toBe(contentUrl(item.id, { download: true }));
    for (const tag of ['img', 'video', 'audio']) expect(stage.querySelector(tag)).toBeNull();
    expect(hasSpinner(stage)).toBe(false);
    expect(settled).toEqual([item.id]);
  });
});

describe('a file that is not known', () => {
  it('shows a spinner over an empty picture while its row is awaited', async () => {
    const { stage, settled } = await open(undefined);

    expect(hasSpinner(stage)).toBe(true);
    expect(stage.getAttribute('aria-busy')).toBe('true');
    expect(img(stage).hasAttribute('src')).toBe(false);
    expect(settled).toEqual([]);
  });

  it('says so when it is gone', async () => {
    const { stage, settled } = await open(undefined, { failure: 'missing' });

    expect(text(stage)).toContain('This file is no longer available');
    expect(part(stage, 'apm-icon[name="image-off"]')).toBeTruthy();
    expect(hasSpinner(stage)).toBe(false);
    expect(stage.getAttribute('aria-busy')).toBe('false');
    expect(settled).toEqual([]);
  });

  it('offers Retry when it could not be fetched', async () => {
    const { stage } = await open(undefined, { failure: 'failed' });
    const retries: Event[] = [];
    stage.addEventListener('stage-retry', (event) => retries.push(event));

    expect(text(alertOf(stage))).toContain('This file could not be loaded');
    part<HTMLButtonElement>(alertOf(stage), 'button').click();

    expect(retries).toHaveLength(1);
    expect(retries[0]?.bubbles).toBe(false);
  });

  it('shows the file once it is known', async () => {
    const item = photo();
    const { stage } = await open(undefined, { failure: 'failed' });

    await update(stage, { item, failure: undefined });

    expect(img(stage).getAttribute('src')).toBe(thumbUrl(item, 'lg'));
    expect(stage.querySelector('[data-stage-error]')).toBeNull();
    expect(FakeImage.requested(previewUrl(item.id))).toHaveLength(1);
  });
});

describe('release', () => {
  it('abandons the preview and empties a stage that is still loading', async () => {
    const item = photo();
    const { stage, settled } = await open(item);
    const [request] = FakeImage.requested(previewUrl(item.id));

    stage.release();
    await flush(stage);

    expect(request?.src).toBe('');
    expect(img(stage).hasAttribute('src')).toBe(false);
    request?.finish();
    await flush(stage);
    expect(settled).toEqual([]);
  });

  it('leaves a picture that is shown on screen while the dialog fades out', async () => {
    const item = photo();
    const { stage } = await open(item);
    await finish(stage, previewUrl(item.id));

    stage.release();
    await flush(stage);

    expect(img(stage).getAttribute('src')).toBe(previewUrl(item.id));
  });

  it('starts nothing for a file that arrives afterwards', async () => {
    const { stage, settled } = await open(photo());
    stage.release();
    const before = FakeImage.instances.length;

    await update(stage, { item: photo({ id: 2 }) });

    expect(FakeImage.instances).toHaveLength(before);
    expect(settled).toEqual([]);
  });

  it('happens when the stage leaves the page', async () => {
    const item = photo();
    const { stage } = await open(item);
    const [request] = FakeImage.requested(previewUrl(item.id));

    stage.remove();

    expect(request?.src).toBe('');
  });
});
