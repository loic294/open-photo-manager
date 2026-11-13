import { afterEach, describe, expect, it, vi } from 'vitest';
import type { FileItem } from '@/api/generated/FileItem';
import type { Overlay } from '@/api/generated/Overlay';
import {
  CARD_CAPTION_PX,
  type CardActivateDetail,
  cardBadges,
  cardFrameClasses,
  cardHeight,
  type OpmFileCard,
} from '@/components/opm-file-card';
import { fileItem } from '@/test/file-item';
import { mount } from '@/test/helpers';
import { EM_DASH, formatCaptureDate } from '@/util/format';

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function part<T extends Element = HTMLElement>(root: ParentNode, selector: string): T {
  const found = root.querySelector<T>(selector);
  if (!found) throw new Error(`No element matches "${selector}"`);
  return found;
}

const option = (card: OpmFileCard) => part(card, '[role="option"]');
const image = (card: OpmFileCard) => part<HTMLImageElement>(card, 'img');
const hasSkeleton = (card: OpmFileCard) => card.querySelector('[data-thumb-skeleton]') !== null;

function record(card: OpmFileCard): CardActivateDetail[] {
  const seen: CardActivateDetail[] = [];
  card.addEventListener('card-activate', (event) => seen.push(event.detail));
  return seen;
}

function click(target: Element, init: MouseEventInit = {}): MouseEvent {
  const event = new MouseEvent('click', { bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(event);
  return event;
}

function press(target: Element, init: KeyboardEventInit): KeyboardEvent {
  const event = new KeyboardEvent('keydown', { bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(event);
  return event;
}

const base: Overlay = { matched: false, planned: false, will_delete: false };

describe('cardHeight', () => {
  it('adds the caption to a square thumbnail area', () => {
    expect(CARD_CAPTION_PX).toBe(40);
    expect(cardHeight(160)).toBe(200);
    expect(cardHeight(96)).toBe(136);
  });
});

describe('cardFrameClasses', () => {
  // selected, will delete, matched: the strongest state takes the border, the next one a ring.
  const real: Array<[boolean, boolean, boolean, string]> = [
    [false, false, false, 'border-base-300 bg-base-100'],
    [false, false, true, 'border-success bg-base-100'],
    [false, true, false, 'border-error bg-base-100'],
    [false, true, true, 'border-error bg-base-100 ring-2 ring-inset ring-success'],
    [true, false, false, 'border-primary bg-primary/10'],
    [true, false, true, 'border-primary bg-primary/10 ring-2 ring-inset ring-success'],
    [true, true, false, 'border-primary bg-primary/10 ring-2 ring-inset ring-error'],
    [true, true, true, 'border-primary bg-primary/10 ring-2 ring-inset ring-error'],
  ];

  it.each(real)('selected=%s will_delete=%s matched=%s', (selected, willDelete, matched, want) => {
    const overlay = { ...base, matched, will_delete: willDelete };

    expect(cardFrameClasses(overlay, selected)).toBe(want);
  });

  // A ghost is never selected, so the same four rows hold whatever `selected` says.
  const ghost: Array<[boolean, boolean, string]> = [
    [false, false, 'border-base-300 bg-base-100 border-dashed opacity-60'],
    [false, true, 'border-success bg-base-100 border-dashed opacity-60'],
    [true, false, 'border-error bg-base-100 border-dashed opacity-60'],
    [
      true,
      true,
      'border-error bg-base-100 ring-2 ring-inset ring-success border-dashed opacity-60',
    ],
  ];

  describe.each([false, true])('a ghost, selected=%s', (selected) => {
    it.each(ghost)('will_delete=%s matched=%s', (willDelete, matched, want) => {
      const overlay = { planned: true, matched, will_delete: willDelete };

      expect(cardFrameClasses(overlay, selected)).toBe(want);
    });
  });
});

describe('cardBadges', () => {
  it('has none for a plain image', () => {
    expect(cardBadges(fileItem())).toEqual([]);
  });

  it('says a ghost is new', () => {
    const [badge] = cardBadges(fileItem({ overlay: { planned: true } }));

    expect(badge).toMatchObject({ id: 'new', text: 'new', variant: 'primary', place: 'top' });
  });

  it('shows the rating as a star and a number', () => {
    const [one] = cardBadges(fileItem({ rating: 1 }));
    const [five] = cardBadges(fileItem({ rating: 5 }));

    expect(one).toMatchObject({ id: 'rating', icon: 'star', text: '1', label: '1 star' });
    expect(five).toMatchObject({ text: '5', label: '5 stars', variant: 'warning' });
  });

  it('shows no rating badge for an unrated file', () => {
    expect(cardBadges(fileItem({ rating: 0 })).map((badge) => badge.id)).toEqual([]);
  });

  it('tells a selected flag from a rejected one', () => {
    const [selected] = cardBadges(fileItem({ flag: 1 }));
    const [rejected] = cardBadges(fileItem({ flag: -1 }));

    expect(selected).toMatchObject({ id: 'flag', label: 'Flagged selected', variant: 'success' });
    expect(rejected).toMatchObject({ id: 'flag', label: 'Flagged rejected', variant: 'error' });
  });

  it('shows that Immich has the file', () => {
    const [badge] = cardBadges(fileItem({ in_immich: true }));

    expect(badge).toMatchObject({
      id: 'immich',
      icon: 'cloud',
      label: 'In Immich',
      variant: 'info',
    });
  });

  it('puts the duration on a video', () => {
    const [badge] = cardBadges(fileItem({ kind: 'video', duration_ms: 65_000 }));

    expect(badge).toEqual({
      id: 'kind',
      icon: 'play',
      text: '1:05',
      label: 'Video, 1:05',
      variant: 'neutral',
      place: 'bottom',
    });
  });

  it('copes with a video or an audio file that has no duration', () => {
    const [video] = cardBadges(fileItem({ kind: 'video', duration_ms: null }));
    const [audio] = cardBadges(fileItem({ kind: 'audio', duration_ms: null }));

    expect(video).toMatchObject({ icon: 'play', label: 'Video' });
    expect(video?.text).toBeUndefined();
    expect(audio).toMatchObject({ icon: 'music', label: 'Audio' });
    expect(audio?.text).toBeUndefined();
  });

  it('puts the duration on audio', () => {
    const [badge] = cardBadges(fileItem({ kind: 'audio', duration_ms: 3_725_000 }));

    expect(badge).toMatchObject({ icon: 'music', text: '1:02:05', label: 'Audio, 1:02:05' });
  });

  it.each([
    ['sidecar', 'Sidecar file'],
    ['other', 'File'],
  ] as const)('marks a %s file', (kind, label) => {
    const [badge] = cardBadges(fileItem({ kind }));

    expect(badge).toMatchObject({ id: 'kind', icon: 'file', label, place: 'bottom' });
  });

  it('lists them in a fixed order', () => {
    const item = fileItem({
      kind: 'video',
      duration_ms: 5_000,
      rating: 3,
      flag: 1,
      in_immich: true,
      overlay: { planned: true },
    });

    expect(cardBadges(item).map((badge) => badge.id)).toEqual([
      'new',
      'rating',
      'flag',
      'immich',
      'kind',
    ]);
  });
});

describe('opm-file-card', () => {
  describe('layout', () => {
    it('fills the cell it is given', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });

      expect(card.classList.contains('block')).toBe(true);
      expect(card.classList.contains('h-full')).toBe(true);
      expect(card.classList.contains('w-full')).toBe(true);
    });

    it('is a DaisyUI card with the name and the date under the thumbnail', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });

      expect(option(card).classList.contains('card')).toBe(true);
      expect(card.querySelector('figure')).not.toBeNull();
      expect(part(card, '[data-name]').textContent).toBe('IMG_0007.jpg');
    });

    it('shows the full filename in a DaisyUI tooltip', async () => {
      const card = await mount('opm-file-card', { item: fileItem({ size: 4_200_000 }) });

      const name = part(card, '[data-name]');
      expect(name.classList.contains('tooltip')).toBe(true);
      expect(name.getAttribute('data-tip')).toBe('IMG_0007.jpg');
    });
  });

  describe('capture date', () => {
    const ts = Date.UTC(2024, 0, 5, 15, 4) / 1000;

    it('shows the wall clock of the photographer', async () => {
      const card = await mount('opm-file-card', { item: fileItem({ capture_ts: ts }) });

      const text = part(card, '[data-date] span').textContent;
      expect(text).toBe(formatCaptureDate(ts));
      expect(text).toContain('2024');
    });

    it('shows the capture time and file size in the date tooltip', async () => {
      const card = await mount('opm-file-card', {
        item: fileItem({ capture_ts: ts, size: 4_200_000 }),
      });
      const date = part(card, '[data-date]');
      expect(date.classList.contains('tooltip')).toBe(true);
      expect(date.getAttribute('data-tip')).toContain(formatCaptureDate(ts));
      expect(date.getAttribute('data-tip')).toContain('4 MB');
    });

    it.each(['exif', 'xmp', 'video'] as const)(
      'marks a date from %s metadata with a camera',
      async (src) => {
        const item = fileItem({ capture_ts: ts, capture_src: src });
        const card = await mount('opm-file-card', { item });

        expect(part(card, '[data-date] opm-icon').getAttribute('name')).toBe('camera');
      },
    );

    it.each(['fs', null] as const)('marks a file date (source %s) with a clock', async (src) => {
      const item = fileItem({ capture_ts: ts, capture_src: src });
      const card = await mount('opm-file-card', { item });

      const hint = part(card, '[data-date]').getAttribute('data-tip');
      expect(part(card, '[data-date] opm-icon').getAttribute('name')).toBe('clock');
      expect(hint).toContain('File date');
    });

    it('shows a dash and no icon when there is no date', async () => {
      const item = fileItem({ capture_ts: null, capture_src: null });
      const card = await mount('opm-file-card', { item });

      expect(part(card, '[data-date] span').textContent).toBe(EM_DASH);
      expect(card.querySelector('[data-date] opm-icon')).toBeNull();
    });
  });

  describe('thumbnail', () => {
    it('asks for the small rendition of a small card, versioned by the file time', async () => {
      vi.stubGlobal('devicePixelRatio', 1);
      const item = fileItem({ id: 7, mtime: 1_700_000_000 });
      const card = await mount('opm-file-card', { item });

      expect(image(card).getAttribute('src')).toBe('/api/files/7/thumb?size=sm&v=1700000000');
    });

    it('asks for the large rendition when the card is big or the screen dense', async () => {
      vi.stubGlobal('devicePixelRatio', 2);
      const dense = await mount('opm-file-card', { item: fileItem(), size: 200 });
      vi.stubGlobal('devicePixelRatio', 1);
      const big = await mount('opm-file-card', { item: fileItem(), size: 320 });
      const bigger = await mount('opm-file-card', { item: fileItem(), size: 400 });

      expect(image(dense).getAttribute('src')).toContain('size=lg');
      expect(image(big).getAttribute('src')).toContain('size=sm');
      expect(image(bigger).getAttribute('src')).toContain('size=lg');
    });

    it('draws a ghost from the real file it stands for', async () => {
      const item = fileItem({ id: 12, mtime: 5, overlay: { planned: true } });
      const card = await mount('opm-file-card', { item });

      expect(image(card).getAttribute('src')).toBe('/api/files/12/thumb?size=sm&v=5');
    });

    it('loads lazily, without a text alternative or a drag', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });

      const picture = image(card);
      expect(picture.getAttribute('loading')).toBe('lazy');
      expect(picture.getAttribute('decoding')).toBe('async');
      expect(picture.getAttribute('alt')).toBe('');
      expect(picture.getAttribute('draggable')).toBe('false');
    });

    it('gives videos a thumbnail too', async () => {
      const card = await mount('opm-file-card', { item: fileItem({ kind: 'video' }) });

      expect(card.querySelector('img')).not.toBeNull();
    });

    it.each([
      ['audio', 'music'],
      ['sidecar', 'file'],
      ['other', 'file'],
    ] as const)('shows an icon, and requests nothing, for %s', async (kind, icon) => {
      const card = await mount('opm-file-card', { item: fileItem({ kind }) });

      expect(card.querySelector('img')).toBeNull();
      expect(part(card, 'figure opm-icon').getAttribute('name')).toBe(icon);
    });

    it('shows a skeleton until the picture has loaded', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });
      expect(hasSkeleton(card)).toBe(true);

      image(card).dispatchEvent(new Event('load'));
      await card.updateComplete;

      expect(hasSkeleton(card)).toBe(false);
      expect(card.querySelector('img')).not.toBeNull();
    });

    it('keeps the loaded picture while another rendition loads', async () => {
      vi.stubGlobal('devicePixelRatio', 1);
      const card = await mount('opm-file-card', { item: fileItem() });
      image(card).dispatchEvent(new Event('load'));
      await card.updateComplete;

      card.size = 400;
      await card.updateComplete;

      expect(image(card).getAttribute('src')).toContain('size=lg');
      expect(hasSkeleton(card)).toBe(false);
    });

    it('starts over with a skeleton for another file', async () => {
      const card = await mount('opm-file-card', { item: fileItem({ id: 7 }) });
      image(card).dispatchEvent(new Event('load'));
      await card.updateComplete;

      card.item = fileItem({ id: 8 });
      await card.updateComplete;

      expect(hasSkeleton(card)).toBe(true);
    });

    it('falls back to an icon when the server cannot render the picture', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });

      image(card).dispatchEvent(new Event('error'));
      await card.updateComplete;

      expect(card.querySelector('img')).toBeNull();
      expect(hasSkeleton(card)).toBe(false);
      expect(part(card, 'figure opm-icon').getAttribute('name')).toBe('image-off');
    });

    it('tries again for another file after a failure', async () => {
      const card = await mount('opm-file-card', { item: fileItem({ id: 7 }) });
      image(card).dispatchEvent(new Event('error'));
      await card.updateComplete;

      card.item = fileItem({ id: 8 });
      await card.updateComplete;

      expect(image(card).getAttribute('src')).toContain('/api/files/8/');
    });

    it('ignores a load or an error of a request that was dropped', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });
      const picture = image(card);
      picture.removeAttribute('src');

      picture.dispatchEvent(new Event('error'));
      picture.dispatchEvent(new Event('load'));
      await card.updateComplete;

      expect(hasSkeleton(card)).toBe(true);
      expect(card.querySelector('img')).not.toBeNull();
    });
  });

  describe('releasing the thumbnail request', () => {
    it('drops it when the card leaves the DOM and takes it up again when it returns', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });
      const url = image(card).getAttribute('src');
      image(card).dispatchEvent(new Event('load'));
      await card.updateComplete;

      card.remove();
      await Promise.resolve();
      expect(image(card).hasAttribute('src')).toBe(false);

      document.body.append(card);
      await card.updateComplete;
      expect(image(card).getAttribute('src')).toBe(url);
      expect(hasSkeleton(card)).toBe(true);
    });

    it('keeps it when the card is only moved', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });
      const url = image(card).getAttribute('src');

      card.remove();
      document.body.append(card);
      await Promise.resolve();
      await card.updateComplete;

      expect(image(card).getAttribute('src')).toBe(url);
    });

    it('has nothing to release for a card without a picture', async () => {
      const card = await mount('opm-file-card', { item: fileItem({ kind: 'audio' }) });

      card.remove();
      await Promise.resolve();
      document.body.append(card);
      await card.updateComplete;

      expect(card.querySelector('img')).toBeNull();
    });
  });

  describe('badges', () => {
    it('renders every badge as a labelled image', async () => {
      const item = fileItem({
        rating: 3,
        flag: -1,
        in_immich: true,
        kind: 'video',
        duration_ms: 65_000,
      });
      const card = await mount('opm-file-card', { item });

      const badges = [...card.querySelectorAll('[data-badge]')];
      expect(badges.map((badge) => badge.getAttribute('data-badge'))).toEqual([
        'rating',
        'flag',
        'immich',
        'kind',
      ]);
      expect(badges.map((badge) => badge.getAttribute('aria-label'))).toEqual([
        '3 stars',
        'Flagged rejected',
        'In Immich',
        'Video, 1:05',
      ]);
      for (const badge of badges) {
        expect(badge.getAttribute('role')).toBe('img');
        expect(badge.getAttribute('title')).toBe(badge.getAttribute('aria-label'));
        expect(badge.classList.contains('badge')).toBe(true);
      }
    });

    it('puts the state badges at the top and the kind at the bottom', async () => {
      const item = fileItem({ rating: 3, kind: 'video', duration_ms: 1000 });
      const card = await mount('opm-file-card', { item });

      const rating = part(card, '[data-badge="rating"]');
      const kind = part(card, '[data-badge="kind"]');
      expect(rating.parentElement?.classList.contains('top-1')).toBe(true);
      expect(kind.parentElement?.classList.contains('bottom-1')).toBe(true);
      expect(kind.textContent).toContain('0:01');
    });

    it('colours a badge by what it says', async () => {
      const card = await mount('opm-file-card', { item: fileItem({ rating: 2, flag: 1 }) });

      expect(part(card, '[data-badge="rating"]').classList.contains('badge-warning')).toBe(true);
      expect(part(card, '[data-badge="flag"]').classList.contains('badge-success')).toBe(true);
    });

    it('renders none for a plain image', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });

      expect(card.querySelector('[data-badge]')).toBeNull();
    });
  });

  describe('accessible name and state', () => {
    it('reads the name, the date and everything the badges say', async () => {
      const ts = Date.UTC(2024, 0, 5, 15, 4) / 1000;
      const item = fileItem({
        capture_ts: ts,
        rating: 3,
        flag: 1,
        in_immich: true,
        overlay: { will_delete: true, matched: true },
      });
      const card = await mount('opm-file-card', { item });

      expect(option(card).getAttribute('aria-label')).toBe(
        `IMG_0007.jpg, ${formatCaptureDate(ts)}, will be deleted, matched, 3 stars, Flagged selected, In Immich`,
      );
    });

    it('says when there is no date', async () => {
      const card = await mount('opm-file-card', { item: fileItem({ capture_ts: null }) });

      expect(option(card).getAttribute('aria-label')).toBe('IMG_0007.jpg, no date');
    });

    it('is an option that reports whether it is selected', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });
      expect(option(card).getAttribute('aria-selected')).toBe('false');

      card.selected = true;
      await card.updateComplete;

      expect(option(card).getAttribute('aria-selected')).toBe('true');
    });

    it('reports its place in the list once the list size is known', async () => {
      const card = await mount('opm-file-card', { item: fileItem(), index: 4 });
      expect(option(card).hasAttribute('aria-posinset')).toBe(false);
      expect(option(card).hasAttribute('aria-setsize')).toBe(false);

      card.setSize = 50_000;
      await card.updateComplete;
      expect(option(card).getAttribute('aria-posinset')).toBe('5');
      expect(option(card).getAttribute('aria-setsize')).toBe('50000');

      card.index = -1;
      await card.updateComplete;
      expect(option(card).hasAttribute('aria-posinset')).toBe(false);
      expect(option(card).hasAttribute('aria-setsize')).toBe(false);
    });
  });

  describe('frame', () => {
    it('draws the selection with a primary border', async () => {
      const card = await mount('opm-file-card', { item: fileItem(), selected: true });

      expect(option(card).classList.contains('border-primary')).toBe(true);
      expect(option(card).classList.contains('bg-primary/10')).toBe(true);
    });

    it('draws the states of the plan', async () => {
      const deleted = await mount('opm-file-card', {
        item: fileItem({ overlay: { will_delete: true } }),
      });
      const matched = await mount('opm-file-card', {
        item: fileItem({ overlay: { matched: true } }),
      });

      expect(option(deleted).classList.contains('border-error')).toBe(true);
      expect(option(matched).classList.contains('border-success')).toBe(true);
    });

    it('keeps a second state visible with a ring', async () => {
      const item = fileItem({ overlay: { will_delete: true, matched: true } });
      const card = await mount('opm-file-card', { item });

      expect(option(card).classList.contains('border-error')).toBe(true);
      expect(option(card).classList.contains('ring-success')).toBe(true);
    });

    it('is neutral without a state', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });

      expect(option(card).classList.contains('border-base-300')).toBe(true);
      expect(option(card).classList.contains('cursor-pointer')).toBe(true);
    });
  });

  describe('selection checkbox', () => {
    it('is hidden until the card is hovered or focused', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });

      const classes = part(card, '[data-checkbox]').classList;
      expect(classes.contains('opacity-0')).toBe(true);
      expect(classes.contains('opacity-100')).toBe(false);
      expect(classes.contains('pointer-events-none')).toBe(true);
      expect(classes.contains('group-hover:opacity-100')).toBe(true);
      expect(classes.contains('group-focus-visible:opacity-100')).toBe(true);
      expect(classes.contains('[@media(hover:none)]:opacity-100')).toBe(true);
    });

    it('is shown on every card while a selection is being made', async () => {
      const card = await mount('opm-file-card', { item: fileItem(), selectionActive: true });

      const classes = part(card, '[data-checkbox]').classList;
      expect(classes.contains('opacity-100')).toBe(true);
      expect(classes.contains('opacity-0')).toBe(false);
    });

    it('reads the selection-active attribute', async () => {
      const card = document.createElement('opm-file-card');
      card.setAttribute('selection-active', '');
      card.item = fileItem();
      await mount(card);

      expect(card.selectionActive).toBe(true);
      expect(part(card, '[data-checkbox]').classList.contains('opacity-100')).toBe(true);
    });

    it('is shown on a selected card', async () => {
      const card = await mount('opm-file-card', { item: fileItem(), selected: true });

      expect(part(card, '[data-checkbox]').classList.contains('opacity-100')).toBe(true);
      expect(part(card, '.checkbox').getAttribute('aria-checked')).toBe('true');
    });

    it('is a non-focusable indicator; the option names and announces selection', async () => {
      const card = await mount('opm-file-card', { item: fileItem({ name: 'beach.jpg' }) });
      const box = part(card, 'span.checkbox');
      expect(box.getAttribute('aria-checked')).toBe('false');
      expect(box.getAttribute('aria-hidden')).toBe('true');
      expect(box.hasAttribute('tabindex')).toBe(false);
      expect(option(card).getAttribute('aria-label')).toContain('beach.jpg');
      expect(
        option(card).querySelector('input, button, a[href], [tabindex], [role="checkbox"]'),
      ).toBeNull();
      expect(box.tabIndex).toBe(-1);

      card.selected = true;
      await card.updateComplete;
      expect(box.getAttribute('aria-checked')).toBe('true');
      expect(option(card).getAttribute('aria-selected')).toBe('true');

      card.selected = false;
      await card.updateComplete;
      expect(box.getAttribute('aria-checked')).toBe('false');
      expect(option(card).getAttribute('aria-selected')).toBe('false');
    });

    it('has none on a ghost', async () => {
      const card = await mount('opm-file-card', {
        item: fileItem({ overlay: { planned: true } }),
        selectionActive: true,
      });

      expect(card.querySelector('[data-checkbox]')).toBeNull();
      expect(card.querySelector('input')).toBeNull();
    });
  });

  describe('card-activate', () => {
    it('reports a click on the card with its id and index', async () => {
      const card = await mount('opm-file-card', { item: fileItem({ id: 42 }), index: 5 });
      const seen = record(card);

      click(option(card));

      expect(seen).toEqual([{ id: 42, index: 5, via: 'card', shift: false, toggle: false }]);
    });

    it('bubbles to the pane that owns the grid', async () => {
      const card = await mount('opm-file-card', { item: fileItem({ id: 3 }), index: 1 });
      const seen: Event[] = [];
      const listen = (event: Event) => seen.push(event);
      document.body.addEventListener('card-activate', listen);

      click(option(card));
      document.body.removeEventListener('card-activate', listen);

      expect(seen).toHaveLength(1);
      expect(seen[0]?.bubbles).toBe(true);
      expect((seen[0] as CustomEvent<CardActivateDetail>).detail.id).toBe(3);
    });

    it('reports an index of -1 outside a grid', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });
      const seen = record(card);

      click(option(card));

      expect(seen[0]?.index).toBe(-1);
    });

    it('reports Shift as a range', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });
      const seen = record(card);

      click(option(card), { shiftKey: true });

      expect(seen[0]).toMatchObject({ via: 'card', shift: true, toggle: false });
    });

    it.each([
      ['Ctrl', { ctrlKey: true }],
      ['Cmd', { metaKey: true }],
    ] as const)('reports %s as a toggle', async (_name, modifier) => {
      const card = await mount('opm-file-card', { item: fileItem() });
      const seen = record(card);

      click(option(card), modifier);

      expect(seen[0]).toMatchObject({ via: 'card', shift: false, toggle: true });
    });

    it.each(['.checkbox', '[data-checkbox]', '[data-checkbox] > span'])(
      'reports a click on %s once, as the checkbox',
      async (selector) => {
        const card = await mount('opm-file-card', { item: fileItem({ id: 9 }), index: 2 });
        const seen = record(card);

        click(part(card, selector), { shiftKey: true });

        expect(seen).toEqual([{ id: 9, index: 2, via: 'checkbox', shift: true, toggle: false }]);
      },
    );

    it('leaves the checkbox state to the owner of the selection', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });
      const box = part(card, '.checkbox');

      const event = click(box);

      expect(event.defaultPrevented).toBe(true);
      expect(box.getAttribute('aria-checked')).toBe('false');
    });

    it('keeps the focus on the card when the checkbox is pressed', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });

      const event = new MouseEvent('mousedown', { bubbles: true, cancelable: true });
      part(card, '.checkbox').dispatchEvent(event);

      expect(event.defaultPrevented).toBe(true);
    });

    it('moves the focus to the card when its checkbox is clicked', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });

      click(part(card, '.checkbox'));

      expect(document.activeElement).toBe(option(card));
    });

    it('reports Space and Enter from the card itself', async () => {
      const card = await mount('opm-file-card', { item: fileItem({ id: 4 }), index: 8 });
      const seen = record(card);

      const space = press(option(card), { key: ' ' });
      const enter = press(option(card), { key: 'Enter' });

      expect(seen).toEqual([
        { id: 4, index: 8, via: 'space', shift: false, toggle: false },
        { id: 4, index: 8, via: 'enter', shift: false, toggle: false },
      ]);
      expect(space.defaultPrevented).toBe(true);
      expect(enter.defaultPrevented).toBe(true);
    });

    it('reports the modifiers of a key press', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });
      const seen = record(card);

      press(option(card), { key: ' ', shiftKey: true });
      press(option(card), { key: ' ', ctrlKey: true });
      press(option(card), { key: 'Enter', metaKey: true });

      expect(seen.map(({ shift, toggle }) => [shift, toggle])).toEqual([
        [true, false],
        [false, true],
        [false, true],
      ]);
    });

    it('does not repeat while a key is held, but still keeps the page from scrolling', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });
      const seen = record(card);

      const event = press(option(card), { key: ' ', repeat: true });

      expect(seen).toEqual([]);
      expect(event.defaultPrevented).toBe(true);
    });

    it('ignores keys it does not handle, and Alt combinations', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });
      const seen = record(card);

      const arrow = press(option(card), { key: 'ArrowRight' });
      const letter = press(option(card), { key: 'a' });
      const alt = press(option(card), { key: 'Enter', altKey: true });

      expect(seen).toEqual([]);
      expect(arrow.defaultPrevented).toBe(false);
      expect(letter.defaultPrevented).toBe(false);
      expect(alt.defaultPrevented).toBe(false);
    });

    it('ignores synthetic key events from the decorative indicator', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });
      const seen = record(card);

      press(part(card, '.checkbox'), { key: ' ' });

      expect(seen).toEqual([]);
    });
  });

  describe('ghost', () => {
    const ghost = () => fileItem({ overlay: { planned: true } });

    it('is inert: a click or a key press reports nothing', async () => {
      const card = await mount('opm-file-card', { item: ghost() });
      const seen = record(card);

      click(option(card));
      press(option(card), { key: ' ' });
      press(option(card), { key: 'Enter' });

      expect(seen).toEqual([]);
    });

    it('is disabled, dimmed and dashed, and says it is new', async () => {
      const card = await mount('opm-file-card', { item: ghost() });

      const classes = option(card).classList;
      expect(option(card).getAttribute('aria-disabled')).toBe('true');
      expect(classes.contains('opacity-60')).toBe(true);
      expect(classes.contains('border-dashed')).toBe(true);
      expect(classes.contains('cursor-default')).toBe(true);
      expect(part(card, '[data-badge="new"]').textContent).toBe('new');
    });

    it('cannot be selected', async () => {
      const card = await mount('opm-file-card', { item: ghost(), selected: true });

      expect(option(card).getAttribute('aria-selected')).toBe('false');
      expect(option(card).classList.contains('border-primary')).toBe(false);
    });

    it('is not disabled when it is a real file', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });

      expect(option(card).hasAttribute('aria-disabled')).toBe(false);
    });
  });

  describe('loading placeholder', () => {
    it('shows skeletons when there is no item yet', async () => {
      const card = await mount('opm-file-card');

      expect(option(card).getAttribute('aria-busy')).toBe('true');
      expect(option(card).getAttribute('aria-label')).toBe('Loading');
      expect(card.querySelectorAll('.skeleton').length).toBeGreaterThanOrEqual(3);
      expect(card.querySelector('img')).toBeNull();
      expect(card.querySelector('[data-checkbox]')).toBeNull();
      expect(card.querySelector('[data-badge]')).toBeNull();
    });

    it('reports nothing', async () => {
      const card = await mount('opm-file-card', { index: 3 });
      const seen = record(card);

      click(option(card));
      press(option(card), { key: 'Enter' });

      expect(seen).toEqual([]);
      expect(option(card).getAttribute('aria-selected')).toBe('false');
    });

    it('turns into the file when it arrives, in the same element', async () => {
      const card = await mount('opm-file-card', { index: 3 });
      const before = option(card);

      card.item = fileItem({ name: 'arrived.jpg' });
      await card.updateComplete;

      expect(option(card)).toBe(before);
      expect(before.hasAttribute('aria-busy')).toBe(false);
      expect(part(card, '[data-name]').textContent).toBe('arrived.jpg');
      expect(card.querySelector('img')).not.toBeNull();
    });

    it('goes back to a placeholder when the item is cleared', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });

      card.item = undefined;
      await card.updateComplete;

      expect(option(card).getAttribute('aria-busy')).toBe('true');
      expect(card.querySelector('img')).toBeNull();
    });
  });

  describe('focus', () => {
    it('focuses the option inside, not the host', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });

      card.focus();

      expect(document.activeElement).toBe(option(card));
    });

    it('focuses once rendered when asked before the first render', async () => {
      const card = document.createElement('opm-file-card');
      card.item = fileItem();
      card.focus();

      await mount(card);
      await Promise.resolve();

      expect(document.activeElement).toBe(option(card));
    });

    it('is the tab stop only when it is the focused card of the grid', async () => {
      const card = await mount('opm-file-card', { item: fileItem() });
      expect(option(card).getAttribute('tabindex')).toBe('-1');

      card.focused = true;
      await card.updateComplete;

      expect(option(card).getAttribute('tabindex')).toBe('0');
    });

    it('stays focused when its item loads', async () => {
      const card = await mount('opm-file-card', { focused: true });
      card.focus();

      card.item = fileItem();
      await card.updateComplete;

      expect(document.activeElement).toBe(option(card));
    });
  });

  describe('every kind of file', () => {
    it.each(['image', 'video', 'audio', 'sidecar', 'other'] as const satisfies FileItem['kind'][])(
      'renders a %s',
      async (kind) => {
        const card = await mount('opm-file-card', { item: fileItem({ kind }) });

        expect(option(card).getAttribute('aria-label')).toContain('IMG_0007.jpg');
      },
    );
  });
});
