import { afterEach, beforeEach, describe, expect, it, type Mock, vi } from 'vitest';
import { api, type GetOptions } from '@/api/client';
import { ApiError } from '@/api/errors';
import { contentUrl, previewUrl, thumbUrl } from '@/api/files';
import type { FileDetail } from '@/api/generated/FileDetail';
import type { FileItem } from '@/api/generated/FileItem';
import { openViewer, type ViewerSource } from '@/components/apm-viewer';
import type { ApmViewerStage } from '@/components/apm-viewer-stage';
import { viewerInfoOpen } from '@/components/viewer-info';
import { DETAIL_DEBOUNCE_MS, OPEN_GUARD_MS, PRELOAD_MAX_BYTES } from '@/components/viewer-nav';
import { installSelectionShortcuts } from '@/controllers/selection-shortcuts';
import { resetSelectionForTests, selection } from '@/stores/selection';
import { toasts } from '@/stores/toasts';
import { loadVolumes } from '@/stores/volumes';
import { FakeImage } from '@/test/fake-image';
import { fileDetail, fileItem } from '@/test/file-item';

/** Ids that differ from their positions, so a mix-up of the two shows. */
const LIST = [101, 102, 103, 104, 105];

/** What the fake server knows; a file that is not in it is a plain photo. */
const library = new Map<number, FileItem>();

function photo(id: number, overrides: Partial<FileItem> = {}): FileItem {
  return fileItem({
    id,
    name: `IMG_${id}.jpg`,
    kind: 'image',
    mtime: 1_700_000_000 + id,
    ...overrides,
  });
}

function put(item: FileItem): FileItem {
  library.set(item.id, item);
  return item;
}

const itemOf = (id: number): FileItem => library.get(id) ?? photo(id);

interface Deferred<T> {
  promise: Promise<T>;
  resolve(value: T): void;
  reject(error: unknown): void;
}

function defer<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((onResolve, onReject) => {
    resolve = onResolve;
    reject = onReject;
  });
  return { promise, resolve, reject };
}

/** One `GET /api/files/{id}` the viewer made; the test decides when and how it ends. */
interface DetailCall {
  readonly id: number;
  readonly signal: AbortSignal | undefined;
  settled: boolean;
  answer(detail?: FileDetail): void;
  fail(error: unknown): void;
}

let calls: DetailCall[] = [];
/** A real `fetch` rejects once aborted; turn it off to see what a slow answer would do. */
let abortRejects = true;

const callsFor = (id: number): DetailCall[] => calls.filter((call) => call.id === id);

function callFor(id: number): DetailCall {
  const [call] = callsFor(id);
  if (!call) throw new Error(`The viewer did not ask for file ${id}`);
  return call;
}

function answerPending(): void {
  for (const call of calls) {
    if (!call.settled && !call.signal?.aborted) call.answer();
  }
}

function fakeGet(path: string, options?: GetOptions): Promise<unknown> {
  if (path === '/api/volumes') {
    return Promise.resolve([
      {
        id: 1,
        name: 'photos',
        writable: true,
        online: true,
        fs_type: null,
        case_insensitive: null,
        watch_mode: 'watching',
        watch_reason: null,
      },
    ]);
  }
  const id = Number(/^\/api\/files\/(\d+)$/.exec(path)?.[1]);
  if (Number.isNaN(id)) return Promise.reject(new Error(`Unexpected GET ${path}`));
  return new Promise((resolve, reject) => {
    const signal = options?.signal;
    const call: DetailCall = {
      id,
      signal,
      settled: false,
      answer: (detail) => {
        call.settled = true;
        resolve(detail ?? fileDetail(itemOf(id)));
      },
      fail: (error) => {
        call.settled = true;
        reject(error);
      },
    };
    calls.push(call);
    if (abortRejects) {
      signal?.addEventListener(
        'abort',
        () => call.fail(new DOMException('Aborted', 'AbortError')),
        {
          once: true,
        },
      );
    }
  });
}

interface Listing {
  readonly source: ViewerSource;
  readonly loadIds: Mock<(signal: AbortSignal) => Promise<readonly number[]>>;
  readonly ids: Deferred<readonly number[]>;
  /** The signal `loadIds` was given. */
  signal(): AbortSignal | undefined;
}

/** A source starting at `startId`: with its row (`known`) and with the list, unless it comes `later`. */
function listing(
  startId: number,
  options: { known?: boolean; ids?: readonly number[] | 'later' } = {},
): Listing {
  const { known = true, ids = LIST } = options;
  const answer = defer<readonly number[]>();
  if (ids !== 'later') answer.resolve(ids);
  const loadIds = vi.fn<(signal: AbortSignal) => Promise<readonly number[]>>(() => answer.promise);
  return {
    source: { startId, startItem: known ? itemOf(startId) : undefined, loadIds },
    loadIds,
    ids: answer,
    signal: () => loadIds.mock.calls[0]?.[0],
  };
}

function part<T extends Element = HTMLElement>(root: ParentNode, selector: string): T {
  const found = root.querySelector<T>(selector);
  if (!found) throw new Error(`No element matches "${selector}"`);
  return found;
}

const text = (element: Element) => element.textContent?.replace(/\s+/g, ' ').trim();
const stage = () => part<ApmViewerStage>(document, 'apm-viewer-stage');
const root = () => part(document, '[data-viewer-root]');
const dialog = () => part<HTMLDialogElement>(document, 'apm-viewer dialog');
const heading = () => text(part(document, 'apm-viewer h3'));
const position = () => text(part(document, '[data-viewer-position]'));
const status = () => text(part(document, 'apm-viewer [role="status"]'));
const control = <T extends HTMLElement = HTMLButtonElement>(label: string) =>
  part<T>(document, `apm-viewer [aria-label="${label}"]`);

function byText<T extends HTMLElement = HTMLButtonElement>(parent: ParentNode, label: string): T {
  const found = [...parent.querySelectorAll<T>('button, a')].find((one) => text(one) === label);
  if (!found) throw new Error(`No "${label}" button or link`);
  return found;
}

/** Lets every render and promise that the last action set off finish. */
async function settle(rounds = 5): Promise<void> {
  for (let round = 0; round < rounds; round += 1) {
    await Promise.resolve();
    const host = document.querySelector('apm-viewer');
    await host?.updateComplete;
    await host?.querySelector('apm-modal')?.updateComplete;
    await host?.querySelector('apm-viewer-stage')?.updateComplete;
  }
}

interface Opened {
  closed: Promise<void>;
  /** How many times the promise `openViewer` returned has resolved. */
  closes(): number;
}

/** Opens the viewer and, unless `guarded`, lets the opening guard run out. */
async function open(source: ViewerSource, options: { guarded?: boolean } = {}): Promise<Opened> {
  let closes = 0;
  const closed = openViewer(source).then(() => {
    closes += 1;
  });
  await settle();
  if (!options.guarded) await vi.advanceTimersByTimeAsync(OPEN_GUARD_MS);
  return { closed, closes: () => closes };
}

function press(
  name: string,
  init: KeyboardEventInit = {},
  target: EventTarget = root(),
): KeyboardEvent {
  const event = new KeyboardEvent('keydown', {
    key: name,
    bubbles: true,
    cancelable: true,
    composed: true,
    ...init,
  });
  target.dispatchEvent(event);
  return event;
}

async function key(
  name: string,
  init: KeyboardEventInit = {},
  target?: EventTarget,
): Promise<KeyboardEvent> {
  const event = press(name, init, target);
  await settle();
  return event;
}

async function click(element: HTMLElement): Promise<void> {
  element.click();
  await settle();
}

/** The debounce passes and the server answers whatever the viewer then asks for. */
async function arrive(): Promise<void> {
  await vi.advanceTimersByTimeAsync(DETAIL_DEBOUNCE_MS);
  await settle();
  answerPending();
  await settle();
}

/** The browser finishes the picture the stage asked for: the file is on screen. */
async function show(id: number): Promise<void> {
  const [request] = FakeImage.requested(previewUrl(id));
  if (!request) throw new Error(`Nothing asked for the preview of file ${id}`);
  request.finish();
  await settle();
}

/** The user presses Escape: the dialog fires `cancel`. */
async function cancelDialog(): Promise<void> {
  dialog().dispatchEvent(new Event('cancel', { cancelable: true }));
  await settle();
}

const stops: Array<() => void> = [];

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal('Image', FakeImage);
  FakeImage.instances.length = 0;
  library.clear();
  calls = [];
  abortRejects = true;
  localStorage.clear();
  viewerInfoOpen.value = false;
  vi.spyOn(api, 'get').mockImplementation(fakeGet as typeof api.get);
});

afterEach(() => {
  for (const stop of stops.splice(0)) stop();
  for (const host of document.querySelectorAll('apm-viewer')) host.remove();
  for (const stray of document.querySelectorAll('[data-test-opener]')) stray.remove();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  resetSelectionForTests();
});

describe('opening', () => {
  it('shows the file from its listing row at once, without asking the server', async () => {
    const item = put(photo(103, { name: 'sunset.jpg' }));

    await open(listing(103).source);

    expect(heading()).toBe('sunset.jpg');
    expect(calls).toHaveLength(0);
    expect(part<HTMLImageElement>(stage(), 'img').getAttribute('src')).toBe(thumbUrl(item, 'lg'));
    expect(dialog().open).toBe(true);
  });

  it('asks for the detail when it only knows the id, with a placeholder meanwhile', async () => {
    await open(listing(103, { known: false }).source);

    expect(heading()).toBe('Loading…');
    expect(calls.map((call) => call.id)).toEqual([103]);
    expect(stage().item).toBeUndefined();

    answerPending();
    await settle();

    expect(heading()).toBe('IMG_103.jpg');
    expect(stage().item?.id).toBe(103);
  });

  it('counts the position once the list has arrived', async () => {
    const list = listing(103, { ids: 'later' });
    await open(list.source);

    expect(part(document, '[data-viewer-position]').classList.contains('skeleton')).toBe(true);
    expect(document.querySelector('[aria-label="Next file"]')).toBeNull();

    list.ids.resolve(LIST);
    await settle();

    expect(position()).toBe('3 / 5');
    expect(control('Next file')).toBeDefined();
  });

  it('asks for the list once, whatever the user does afterwards', async () => {
    const list = listing(103);
    await open(list.source);

    await key('ArrowRight');
    await arrive();
    await key('i');
    await key('z');
    await key('Home');

    expect(list.loadIds).toHaveBeenCalledTimes(1);
    expect(list.signal()).toBeInstanceOf(AbortSignal);
  });

  it('starts with the focus inside the dialog, on the viewer itself rather than on a button', async () => {
    // A browser focuses the first control of a dialog it opens; happy-dom does not.
    const showModal = HTMLDialogElement.prototype.showModal;
    vi.spyOn(HTMLDialogElement.prototype, 'showModal').mockImplementation(function (
      this: HTMLDialogElement,
    ) {
      showModal.call(this);
      this.querySelector('button')?.focus();
    });

    await open(listing(103).source);

    expect(document.activeElement).toBe(root());
  });

  it('mounts one shared dialog lazily and reuses it', async () => {
    expect(document.querySelectorAll('apm-viewer')).toHaveLength(0);

    await open(listing(103).source);
    await cancelDialog();
    await open(listing(104).source);

    expect(document.querySelectorAll('apm-viewer')).toHaveLength(1);
    expect(heading()).toBe('IMG_104.jpg');
  });

  it('mounts a fresh dialog if the shared one was removed from the page', async () => {
    const first = await open(listing(103).source);
    document.querySelector('apm-viewer')?.remove();
    await first.closed;

    await open(listing(104).source);

    expect(document.querySelectorAll('apm-viewer')).toHaveLength(1);
    expect(dialog().open).toBe(true);
    expect(heading()).toBe('IMG_104.jpg');
  });
});

describe('stepping', () => {
  it.each([
    ['ArrowRight', '4 / 5'],
    ['ArrowLeft', '2 / 5'],
    ['End', '5 / 5'],
    ['Home', '1 / 5'],
  ])('%s moves to %s', async (name, expected) => {
    await open(listing(103).source);

    const event = await key(name);

    expect(position()).toBe(expected);
    expect(event.defaultPrevented).toBe(true);
  });

  it('shows the name of the file it has stepped to once its detail arrives', async () => {
    await open(listing(103).source);

    await key('ArrowRight');
    await arrive();

    expect(heading()).toBe('IMG_104.jpg');
    expect(stage().item?.id).toBe(104);
  });

  it('does not wrap around, and disables the button at each end', async () => {
    await open(listing(101).source);

    expect(control('Previous file').disabled).toBe(true);
    await key('ArrowLeft');
    expect(position()).toBe('1 / 5');

    await key('End');
    expect(control('Next file').disabled).toBe(true);
    await key('ArrowRight');
    expect(position()).toBe('5 / 5');
  });

  it('steps with the on-screen buttons', async () => {
    await open(listing(103).source);

    await click(control('Next file'));
    expect(position()).toBe('4 / 5');
    await click(control('Previous file'));
    await click(control('Previous file'));
    expect(position()).toBe('2 / 5');
  });

  it('leaves out the buttons when there is nothing to step to', async () => {
    await open(listing(103, { ids: [103] }).source);

    expect(position()).toBe('1 / 1');
    expect(document.querySelector('[aria-label="Next file"]')).toBeNull();
    expect(document.querySelector('[aria-label="Previous file"]')).toBeNull();
  });

  it.each([
    ['Cmd', { metaKey: true }],
    ['Ctrl', { ctrlKey: true }],
    ['Alt', { altKey: true }],
  ])('ignores the arrows with %s held', async (_name, modifier) => {
    await open(listing(103).source);

    const event = await key('ArrowRight', modifier);

    expect(position()).toBe('3 / 5');
    expect(event.defaultPrevented).toBe(false);
  });

  it('leaves the arrow keys to a video that has the focus', async () => {
    put(fileItem({ id: 103, kind: 'video', name: 'CLIP_0103.mp4', duration_ms: 5000 }));
    await open(listing(103).source);

    const event = await key('ArrowRight', {}, part(stage(), 'video'));

    expect(position()).toBe('3 / 5');
    expect(event.defaultPrevented).toBe(false);
  });

  it('leaves the arrow keys to an audio player that has the focus', async () => {
    put(fileItem({ id: 103, kind: 'audio', name: 'voice.m4a' }));
    await open(listing(103).source);

    const event = await key('End', {}, part(stage(), 'audio'));

    expect(position()).toBe('3 / 5');
    expect(event.defaultPrevented).toBe(false);
  });

  it('ignores the keys until the list has arrived', async () => {
    const list = listing(103, { ids: 'later' });
    await open(list.source);

    const event = await key('ArrowRight');
    expect(event.defaultPrevented).toBe(false);

    list.ids.resolve(LIST);
    await settle();
    expect(position()).toBe('3 / 5');
  });

  it('takes the focus back when the button it was on has just been disabled', async () => {
    await open(listing(104).source);
    const next = control('Next file');
    next.focus();

    await click(next);

    expect(next.disabled).toBe(true);
    expect(document.activeElement).toBe(root());
  });

  it('takes the focus back after a click on a bare area, so the keys keep working', async () => {
    await open(listing(103).source);
    root().blur();
    expect(document.activeElement).not.toBe(root());

    await click(part(document, 'apm-viewer h3'));
    expect(document.activeElement).toBe(root());

    await key('ArrowRight');
    expect(position()).toBe('4 / 5');
  });
});

describe('stepping quickly', () => {
  it('shows the new file at once as unknown, with the old picture dimmed rather than passed off as the new one', async () => {
    await open(listing(103).source);
    await show(103);
    expect(part(stage(), 'img').classList.contains('opacity-40')).toBe(false);

    await key('ArrowRight');

    expect(heading()).toBe('Loading…');
    expect(position()).toBe('4 / 5');
    expect(stage().item).toBeUndefined();
    expect(stage().getAttribute('aria-busy')).toBe('true');
    expect(stage().querySelector('[data-stage-spinner]')).not.toBeNull();
    expect(part(stage(), 'img').classList.contains('opacity-40')).toBe(true);
    expect(part<HTMLImageElement>(stage(), 'img').alt).toBe('');
  });

  it('asks for a file only after the pause, and never for the ones passed over', async () => {
    await open(listing(101, { known: false }).source);
    answerPending();
    await settle();

    await key('ArrowRight');
    await vi.advanceTimersByTimeAsync(DETAIL_DEBOUNCE_MS - 1);
    await key('ArrowRight');
    await vi.advanceTimersByTimeAsync(DETAIL_DEBOUNCE_MS - 1);
    expect(calls.map((call) => call.id)).toEqual([101]);

    await vi.advanceTimersByTimeAsync(1);

    expect(calls.map((call) => call.id)).toEqual([101, 103]);
  });

  it('abandons the detail request of the file it leaves', async () => {
    await open(listing(103, { known: false }).source);
    const first = callFor(103);

    await key('ArrowRight');

    expect(first.signal?.aborted).toBe(true);
    expect(calls).toHaveLength(1);
  });

  it('ignores a slow answer for a file the user already left, and keeps it for later', async () => {
    abortRejects = false;
    await open(listing(103, { known: false }).source);
    const slow = callFor(103);
    await key('ArrowRight');
    await vi.advanceTimersByTimeAsync(DETAIL_DEBOUNCE_MS);
    await settle();
    expect(callsFor(104)).toHaveLength(1);

    slow.answer();
    await settle();

    expect(heading()).toBe('Loading…');
    expect(stage().item).toBeUndefined();

    callFor(104).answer();
    await settle();

    expect(heading()).toBe('IMG_104.jpg');
    expect(stage().item?.id).toBe(104);

    await key('ArrowLeft');
    expect(heading()).toBe('IMG_103.jpg');
    expect(callsFor(103)).toHaveLength(1);
  });

  it('does not let an answer for an old file replace what the panel shows', async () => {
    abortRejects = false;
    await open(listing(103, { known: false }).source);
    await key('i');
    const slow = callFor(103);
    await key('ArrowRight');
    await arrive();

    slow.answer();
    await settle();
    await key('i');
    await key('i');

    expect(text(part(document, '[data-info-row="name"]'))).toContain('IMG_104.jpg');
  });

  it('keeps the details of the last files it showed, so going back is instant, and forgets the oldest', async () => {
    const ids = Array.from({ length: 30 }, (_, index) => index + 1);
    await open(listing(1, { known: false, ids }).source);
    answerPending();
    await settle();
    for (let step = 1; step < ids.length; step += 1) {
      await key('ArrowRight');
      await arrive();
    }
    expect(position()).toBe('30 / 30');
    expect(calls).toHaveLength(30);

    await key('ArrowLeft');
    expect(heading()).toBe('IMG_29.jpg');
    expect(calls).toHaveLength(30);

    await key('Home');
    await arrive();
    expect(heading()).toBe('IMG_1.jpg');
    expect(callsFor(1)).toHaveLength(2);
  });
});

describe('preloading the next file', () => {
  it('waits until the current picture has been shown', async () => {
    await open(listing(103).source);

    expect(callsFor(104)).toHaveLength(0);
    expect(FakeImage.requested(thumbUrl(itemOf(104), 'lg'))).toHaveLength(0);

    await show(103);

    expect(callsFor(104)).toHaveLength(1);
  });

  it('goes in the direction of travel', async () => {
    await open(listing(103).source);
    await show(103);
    expect(callsFor(104)).toHaveLength(1);
    expect(callsFor(102)).toHaveLength(0);

    await key('ArrowLeft');
    await arrive();
    await show(102);

    expect(callsFor(101)).toHaveLength(1);
    expect(callsFor(103)).toHaveLength(0);
  });

  it('warms the thumbnail, then the preview, of the next photo', async () => {
    await open(listing(103).source);
    await show(103);
    const thumb = thumbUrl(itemOf(104), 'lg');

    callFor(104).answer();
    await settle();

    expect(FakeImage.requested(thumb)).toHaveLength(1);
    expect(FakeImage.requested(previewUrl(104))).toHaveLength(0);

    FakeImage.requested(thumb)[0]?.finish();
    await settle();

    expect(FakeImage.requested(previewUrl(104))).toHaveLength(1);
  });

  it('has the file ready, with no new request, when the user steps to it', async () => {
    await open(listing(103).source);
    await show(103);
    callFor(104).answer();
    await settle();

    await key('ArrowRight');

    expect(heading()).toBe('IMG_104.jpg');
    expect(callsFor(104)).toHaveLength(1);
  });

  it('never warms the preview of a video or of a photo over the size cap', async () => {
    put(fileItem({ id: 104, kind: 'video', name: 'CLIP_0104.mp4', size: 90_000_000, mtime: 9 }));
    put(photo(105, { size: PRELOAD_MAX_BYTES + 1 }));
    await open(listing(103).source);
    await show(103);

    callFor(104).answer();
    await settle();
    FakeImage.requested(thumbUrl(itemOf(104), 'lg'))[0]?.finish();
    await settle();
    expect(FakeImage.requested(previewUrl(104))).toHaveLength(0);

    await key('ArrowRight');
    part(stage(), 'video').dispatchEvent(new Event('loadedmetadata'));
    await settle();
    callFor(105).answer();
    await settle();
    FakeImage.requested(thumbUrl(itemOf(105), 'lg'))[0]?.finish();
    await settle();

    expect(FakeImage.requested(thumbUrl(itemOf(105), 'lg'))).toHaveLength(1);
    expect(FakeImage.requested(previewUrl(105))).toHaveLength(0);
  });

  it('starts when the list arrives after the picture was shown', async () => {
    const list = listing(103, { ids: 'later' });
    await open(list.source);
    await show(103);
    expect(calls).toHaveLength(0);

    list.ids.resolve(LIST);
    await settle();

    expect(callsFor(104)).toHaveLength(1);
  });

  it('drops the request when the user moves on', async () => {
    await open(listing(103).source);
    await show(103);
    const warming = callFor(104);

    await key('ArrowRight');

    expect(warming.signal?.aborted).toBe(true);
  });

  it('stops warming the neighbours when the viewer closes', async () => {
    await open(listing(103).source);
    await show(103);
    callFor(104).answer();
    await settle();
    expect(FakeImage.requested(thumbUrl(itemOf(104), 'lg'))).toHaveLength(1);

    await cancelDialog();

    expect(FakeImage.requested(thumbUrl(itemOf(104), 'lg'))).toHaveLength(0);
  });
});

describe('the opening guard', () => {
  it('swallows pointer actions for OPEN_GUARD_MS, then lets them through', async () => {
    await open(listing(103).source, { guarded: true });
    await show(103);

    part(stage(), 'img').dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    await click(control('100% zoom'));
    expect(stage().zoomed).toBe(false);

    await vi.advanceTimersByTimeAsync(OPEN_GUARD_MS - 1);
    await click(control('100% zoom'));
    expect(stage().zoomed).toBe(false);

    await vi.advanceTimersByTimeAsync(1);
    await click(control('100% zoom'));
    expect(stage().zoomed).toBe(true);
  });

  it('does not hold back the keyboard', async () => {
    await open(listing(103).source, { guarded: true });
    await show(103);

    await key('z');
    await key('ArrowRight');

    expect(position()).toBe('4 / 5');
  });

  it('starts again when the source is replaced', async () => {
    await open(listing(103).source);
    await show(103);
    void openViewer(listing(104).source);
    await settle();
    await show(104);

    await click(control('100% zoom'));
    expect(stage().zoomed).toBe(false);

    await vi.advanceTimersByTimeAsync(OPEN_GUARD_MS);
    await click(control('100% zoom'));
    expect(stage().zoomed).toBe(true);
  });
});

describe('zoom', () => {
  it('toggles 100% with the button, the z key and a double click on the picture', async () => {
    await open(listing(103).source);
    await show(103);
    expect(stage().zoomed).toBe(false);
    expect(control('100% zoom').getAttribute('aria-pressed')).toBe('false');

    await click(control('100% zoom'));
    expect(stage().zoomed).toBe(true);
    expect(control('100% zoom').getAttribute('aria-pressed')).toBe('true');

    await key('z');
    expect(stage().zoomed).toBe(false);

    part(stage(), 'img').dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    await settle();
    expect(stage().zoomed).toBe(true);
    expect(part(stage(), '[data-stage-scroller]').classList.contains('overflow-auto')).toBe(true);
  });

  it('starts over at 100% off for the next file', async () => {
    await open(listing(103).source);
    await show(103);
    await key('z');
    expect(stage().zoomed).toBe(true);

    await key('ArrowRight');

    expect(stage().zoomed).toBe(false);
  });

  it('is for images only: the button is disabled and z does nothing for a video', async () => {
    put(fileItem({ id: 103, kind: 'video', name: 'CLIP_0103.mp4', duration_ms: 5000 }));
    await open(listing(103).source);

    expect(control('100% zoom').disabled).toBe(true);
    await key('z');
    expect(stage().zoomed).toBe(false);
  });
});

describe('each kind of file', () => {
  it('plays a video in a native player, with its poster and without autoplay', async () => {
    const item = put(fileItem({ id: 103, kind: 'video', name: 'CLIP_0103.mp4', mtime: 7 }));
    await open(listing(103).source);

    const video = part<HTMLVideoElement>(stage(), 'video');
    expect(video.hasAttribute('controls')).toBe(true);
    expect(video.hasAttribute('autoplay')).toBe(false);
    expect(video.getAttribute('poster')).toBe(thumbUrl(item, 'lg'));
    expect(video.getAttribute('src')).toBe(previewUrl(103));
    expect(status()).toBe('Video 3 of 5: CLIP_0103.mp4');
  });

  it('offers an audio player', async () => {
    put(fileItem({ id: 103, kind: 'audio', name: 'voice.m4a' }));
    await open(listing(103).source);

    const audio = part<HTMLAudioElement>(stage(), 'audio');
    expect(audio.hasAttribute('controls')).toBe(true);
    expect(audio.getAttribute('src')).toBe(previewUrl(103));
    expect(status()).toBe('File 3 of 5: voice.m4a');
  });

  it.each(['other', 'sidecar'] as const)(
    'says there is no preview for a %s file and offers the download',
    async (kind) => {
      put(fileItem({ id: 103, kind, name: 'notes.txt' }));
      await open(listing(103).source);

      expect(text(stage())).toContain('No preview for this kind of file');
      expect(byText(stage(), 'Download').getAttribute('href')).toBe(
        contentUrl(103, { download: true }),
      );
      expect(FakeImage.instances).toHaveLength(0);
    },
  );

  it('falls back to the poster and the original when the browser cannot play a video', async () => {
    put(fileItem({ id: 103, kind: 'video', name: 'CLIP_0103.mp4', mtime: 7 }));
    await open(listing(103).source);

    part(stage(), 'video').dispatchEvent(new Event('error'));
    await settle();

    expect(text(part(stage(), '[data-stage-error]'))).toContain(
      "Your browser can't play this video",
    );
    expect(byText(stage(), 'Open original').getAttribute('href')).toBe(contentUrl(103));
    expect(byText(stage(), 'Download').getAttribute('href')).toBe(
      contentUrl(103, { download: true }),
    );
    expect(stage().querySelector('video')).toBeNull();
  });

  it('offers Retry when a photo cannot be loaded, and loads it again', async () => {
    await open(listing(103).source);
    FakeImage.requested(previewUrl(103))[0]?.fail();
    await settle();
    expect(text(part(stage(), '[data-stage-error]'))).toContain('This image could not be loaded');
    const before = FakeImage.instances.length;

    await click(byText(stage(), 'Retry'));

    expect(FakeImage.instances.length).toBeGreaterThan(before);
    expect(FakeImage.instances.at(-1)?.src).toBe(previewUrl(103));
  });
});

describe('when something goes wrong', () => {
  it('says the file is gone on a 404, and still steps to its neighbours', async () => {
    await open(listing(103, { known: false }).source);

    callFor(103).fail(new ApiError({ status: 404, code: 'not_found', message: 'No such file' }));
    await settle();

    expect(heading()).toBe('File unavailable');
    expect(text(part(stage(), '[data-stage-missing]'))).toContain(
      'This file is no longer available',
    );
    expect(document.querySelector('[aria-label="Open original"]')).toBeNull();
    expect(document.querySelector('[aria-label="Download"]')).toBeNull();

    await key('ArrowRight');
    await arrive();

    expect(position()).toBe('4 / 5');
    expect(heading()).toBe('IMG_104.jpg');
    expect(document.querySelector('[data-stage-missing]')).toBeNull();
  });

  it('offers Retry when the detail cannot be loaded, and shows the file once it can', async () => {
    await open(listing(103, { known: false }).source);

    callFor(103).fail(new ApiError({ status: 500, code: 'internal', message: 'Boom' }));
    await settle();

    expect(heading()).toBe("Couldn't load the file");
    await key('ArrowRight');
    await key('ArrowLeft');
    await arrive();
    expect(heading()).toBe('IMG_103.jpg');
  });

  it('retries a failed detail from the stage', async () => {
    await open(listing(103, { known: false }).source);
    callFor(103).fail(new ApiError({ status: 0, code: 'network', message: 'Offline' }));
    await settle();
    expect(text(part(stage(), '[data-stage-error]'))).toContain('This file could not be loaded');

    await click(byText(stage(), 'Retry'));
    expect(callsFor(103)).toHaveLength(2);
    answerPending();
    await settle();

    expect(heading()).toBe('IMG_103.jpg');
    expect(stage().item?.id).toBe(103);
  });

  it('shows the one file without stepping when the list cannot be loaded', async () => {
    const list = listing(103, { ids: 'later' });
    await open(list.source);

    list.ids.reject(new Error('offline'));
    await settle();

    expect(text(part(document, '[data-viewer-notice]'))).toBe("Couldn't load the list");
    expect(document.querySelector('[data-viewer-position]')).toBeNull();
    expect(document.querySelector('[aria-label="Next file"]')).toBeNull();
    expect(heading()).toBe('IMG_103.jpg');
    const event = await key('ArrowRight');
    expect(event.defaultPrevented).toBe(false);
    expect(heading()).toBe('IMG_103.jpg');
  });

  it('treats a loadIds that throws like one that rejects', async () => {
    await open({
      startId: 103,
      startItem: itemOf(103),
      loadIds: () => {
        throw new Error('boom');
      },
    });

    expect(text(part(document, '[data-viewer-notice]'))).toBe("Couldn't load the list");
    expect(heading()).toBe('IMG_103.jpg');
  });

  it('shows the one file without stepping when it is not in the list', async () => {
    await open(listing(103, { ids: [101, 102, 104] }).source);

    expect(text(part(document, '[data-viewer-notice]'))).toBe('Not in the list anymore');
    expect(document.querySelector('[aria-label="Next file"]')).toBeNull();
    expect(heading()).toBe('IMG_103.jpg');
  });

  it('never raises a toast for any of it', async () => {
    const list = listing(103, { known: false, ids: 'later' });
    await open(list.source);
    list.ids.reject(new Error('offline'));
    callFor(103).fail(new ApiError({ status: 404, code: 'not_found', message: 'Gone' }));
    await settle();

    expect(toasts.value).toHaveLength(0);
  });
});

describe('the info panel', () => {
  it('opens with i or the button and closes the same way', async () => {
    await open(listing(103).source);
    expect(document.querySelector('aside')).toBeNull();
    expect(control('Info').getAttribute('aria-pressed')).toBe('false');

    await key('i');
    expect(part(document, 'aside[aria-label="File information"]')).toBeDefined();
    expect(control('Info').getAttribute('aria-pressed')).toBe('true');

    await click(control('Info'));
    expect(document.querySelector('aside')).toBeNull();
    expect(control('Info').getAttribute('aria-pressed')).toBe('false');
  });

  it('asks for the detail only once the panel needs it, for a file whose row is known', async () => {
    await open(listing(103).source);
    expect(calls).toHaveLength(0);

    await key('i');

    expect(calls.map((call) => call.id)).toEqual([103]);
  });

  it('shows the rows, with the volume by name, once the detail has arrived', async () => {
    await loadVolumes();
    put(photo(103, { dir: 'trip/day 1', in_immich: true }));
    await open(listing(103).source);
    await key('i');

    callFor(103).answer(
      fileDetail(itemOf(103), {
        hash: 'a'.repeat(64),
        mime: 'image/jpeg',
        tags: ['holiday'],
        sidecars: [{ name: 'IMG_103.xmp', kind: 'xmp' }],
      }),
    );
    await settle();

    const row = (id: string) => text(part(document, `[data-info-row="${id}"]`));
    expect(row('name')).toContain('IMG_103.jpg');
    expect(row('location')).toContain('photos/trip/day 1');
    expect(row('kind')).toContain('image/jpeg');
    expect(row('tags')).toContain('holiday');
    expect(row('sidecars')).toContain('IMG_103.xmp');
    expect(row('hash')).toContain('aaaa');
    expect(row('immich')).toContain('In Immich');
  });

  it('remembers whether it was open', async () => {
    await open(listing(103).source);
    await key('i');
    expect(localStorage.getItem('apm.v1.viewerInfoOpen')).toBe('true');
    await cancelDialog();

    await open(listing(104).source);

    expect(control('Info').getAttribute('aria-pressed')).toBe('true');
    expect(document.querySelector('aside')).not.toBeNull();
    expect(callsFor(104)).toHaveLength(1);
  });

  it('opens on a saved state, before anything is pressed', async () => {
    viewerInfoOpen.value = true;

    await open(listing(103).source);

    expect(document.querySelector('aside')).not.toBeNull();
  });

  it('says so when the file is gone', async () => {
    viewerInfoOpen.value = true;
    await open(listing(103, { known: false }).source);

    callFor(103).fail(new ApiError({ status: 404, code: 'not_found', message: 'Gone' }));
    await settle();

    expect(text(part(document, '[data-info-missing]'))).toBe('This file is no longer available');
  });

  it('offers Retry when the details of a shown file cannot be loaded', async () => {
    viewerInfoOpen.value = true;
    await open(listing(103).source);

    callFor(103).fail(new ApiError({ status: 500, code: 'internal', message: 'Boom' }));
    await settle();
    expect(document.querySelector('[data-info-alert]')).not.toBeNull();
    expect(heading()).toBe('IMG_103.jpg');

    await click(byText(part(document, '[data-info-alert]'), 'Retry'));
    expect(callsFor(103)).toHaveLength(2);
    answerPending();
    await settle();

    expect(document.querySelector('[data-info-alert]')).toBeNull();
  });
});

describe('the toolbar', () => {
  it('links to the original and to its download, for the file shown', async () => {
    await open(listing(103).source);

    const original = control<HTMLAnchorElement>('Open original');
    expect(original.getAttribute('href')).toBe(contentUrl(103));
    expect(original.getAttribute('target')).toBe('_blank');
    expect(original.getAttribute('rel')).toBe('noopener');
    const download = control<HTMLAnchorElement>('Download');
    expect(download.getAttribute('href')).toBe(contentUrl(103, { download: true }));
    expect(download.hasAttribute('download')).toBe(true);

    await key('ArrowRight');

    expect(control('Open original').getAttribute('href')).toBe(contentUrl(104));
    expect(control('Download').getAttribute('href')).toBe(contentUrl(104, { download: true }));
  });
});

describe('closing', () => {
  it('closes on Escape and resolves openViewer exactly once', async () => {
    const opened = await open(listing(103).source);
    expect(opened.closes()).toBe(0);

    await cancelDialog();
    await opened.closed;
    await settle();

    expect(dialog().open).toBe(false);
    expect(opened.closes()).toBe(1);
  });

  it('closes with the X button', async () => {
    const opened = await open(listing(103).source);

    await click(control('Close'));
    await opened.closed;

    expect(dialog().open).toBe(false);
    expect(opened.closes()).toBe(1);
  });

  it('keeps a close request when a pending viewer render runs first', async () => {
    const opened = await open(listing(103).source);
    document.querySelector('apm-viewer')?.requestUpdate();
    control('Close').click();
    await settle();
    expect(dialog().open).toBe(false);
    expect(opened.closes()).toBe(1);
  });

  it('abandons every request and the pictures it still waits for', async () => {
    const list = listing(103, { known: false, ids: 'later' });
    await open(list.source);
    const detail = callFor(103);

    await cancelDialog();

    expect(list.signal()?.aborted).toBe(true);
    expect(detail.signal?.aborted).toBe(true);

    put(photo(103));
    await open(listing(103).source);
    expect(FakeImage.requested(previewUrl(103))).toHaveLength(1);
    await cancelDialog();
    expect(FakeImage.requested(previewUrl(103))).toHaveLength(0);
  });

  it('settles the promise when the dialog is removed from the page', async () => {
    const opened = await open(listing(103).source);

    document.querySelector('apm-viewer')?.remove();
    await opened.closed;

    expect(opened.closes()).toBe(1);
  });

  it('gives focus back to what opened it', async () => {
    const opener = document.createElement('button');
    opener.dataset.testOpener = '';
    document.body.append(opener);
    opener.focus();
    await open(listing(103).source);
    expect(document.activeElement).toBe(root());

    await cancelDialog();

    expect(document.activeElement).toBe(opener);
  });
});

describe('opening again while it is open', () => {
  it('replaces the source, and resolves every promise once, when it closes', async () => {
    const first = listing(103, { ids: 'later' });
    const firstOpened = await open(first.source);
    const second = listing(205, { ids: [201, 205, 209] });
    let secondCloses = 0;
    const secondClosed = openViewer(second.source).then(() => {
      secondCloses += 1;
    });
    await settle();

    expect(heading()).toBe('IMG_205.jpg');
    expect(position()).toBe('2 / 3');
    expect(first.signal()?.aborted).toBe(true);
    expect(second.loadIds).toHaveBeenCalledTimes(1);
    expect(document.querySelectorAll('apm-viewer')).toHaveLength(1);

    first.ids.resolve(LIST);
    await settle();
    expect(position()).toBe('2 / 3');
    expect(firstOpened.closes()).toBe(0);
    expect(secondCloses).toBe(0);

    await cancelDialog();
    await Promise.all([firstOpened.closed, secondClosed]);

    expect(firstOpened.closes()).toBe(1);
    expect(secondCloses).toBe(1);
  });

  it('forgets the zoom, the old requests and the old preloads of the first source', async () => {
    await open(listing(103).source);
    await show(103);
    await key('z');
    const warming = callFor(104);

    void openViewer(listing(201, { ids: [201, 202] }).source);
    await settle();

    expect(stage().zoomed).toBe(false);
    expect(warming.signal?.aborted).toBe(true);
    expect(heading()).toBe('IMG_201.jpg');
  });
});

describe('around the viewer', () => {
  it('leaves the selection alone while it is open (Cmd+A and Escape)', async () => {
    stops.push(installSelectionShortcuts());
    let asked = 0;
    selection.source.setIdsProvider(async () => {
      asked += 1;
      return LIST;
    });
    selection.source.set([101]);
    await open(listing(103).source);

    await key('a', { metaKey: true });
    await key('Escape');

    expect(asked).toBe(0);
    expect([...selection.source.ids.value]).toEqual([101]);
  });

  it('never touches the clipboard, which plain HTTP does not have', async () => {
    const read = vi.fn(() => undefined);
    Object.defineProperty(navigator, 'clipboard', { get: read, configurable: true });
    stops.push(() => Reflect.deleteProperty(navigator, 'clipboard'));
    viewerInfoOpen.value = true;
    await open(listing(103).source);
    callFor(103).answer(fileDetail(itemOf(103), { hash: 'b'.repeat(64) }));
    await settle();

    await click(part(document, '[data-info-row="hash"]'));
    await click(control('Info'));
    await key('i');
    await cancelDialog();

    expect(read).not.toHaveBeenCalled();
  });
});

describe('accessibility', () => {
  it('names the dialog after the file', async () => {
    await open(listing(103).source);

    const labelledBy = dialog().getAttribute('aria-labelledby');
    expect(labelledBy).toBeTruthy();
    expect(text(part(document, `#${labelledBy}`))).toBe('IMG_103.jpg');
  });

  it('labels every control that has no text of its own', async () => {
    await open(listing(103).source);

    for (const one of document.querySelectorAll('apm-viewer button, apm-viewer a')) {
      const named = one.getAttribute('aria-label') || text(one);
      expect(named, one.outerHTML).toBeTruthy();
    }
  });

  it('announces the file and its place, politely', async () => {
    await open(listing(103).source);

    expect(part(document, 'apm-viewer [role="status"]').getAttribute('aria-live')).toBe('polite');
    expect(status()).toBe('Photo 3 of 5: IMG_103.jpg');

    await key('ArrowRight');
    expect(status()).toBe('');
    await arrive();

    expect(status()).toBe('Photo 4 of 5: IMG_104.jpg');
  });

  it('announces just the file when there is no list', async () => {
    await open(listing(103, { ids: [101] }).source);

    expect(status()).toBe('Photo: IMG_103.jpg');
  });

  it('shows the picture with the file name as its text alternative', async () => {
    await open(listing(103).source);

    expect(part<HTMLImageElement>(stage(), 'img').alt).toBe('IMG_103.jpg');
  });
});
