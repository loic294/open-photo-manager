import { afterEach, describe, expect, it, vi } from 'vitest';
import '@/components/opm-folder-list';
import type { FolderMenuEvent, FolderOpenEvent, OpmFolderList } from '@/components/opm-folder-list';
import { folderEntry } from '@/test/folder-entry';
import { mount } from '@/test/helpers';
import { formatCount } from '@/util/format-count';

afterEach(() => {
  vi.restoreAllMocks();
});

function anchors(host: OpmFolderList): HTMLAnchorElement[] {
  return Array.from(host.querySelectorAll<HTMLAnchorElement>('li > a'));
}

function row(host: OpmFolderList, name: string): HTMLAnchorElement {
  const found = anchors(host).find((anchor) => anchor.dataset.name === name);
  if (!found) throw new Error(`no row for "${name}"`);
  return found;
}

function names(host: OpmFolderList): (string | undefined)[] {
  return anchors(host).map((anchor) => anchor.dataset.name);
}

function iconOf(anchor: HTMLElement): string | null {
  return anchor.querySelector('opm-icon')?.getAttribute('name') ?? null;
}

/** The badges of a row, as text, in order. */
function badges(anchor: HTMLElement): string[] {
  return Array.from(anchor.querySelectorAll('.badge')).map(
    (badge) => badge.textContent?.trim() ?? '',
  );
}

function srText(anchor: HTMLElement): string[] {
  return Array.from(anchor.querySelectorAll('.sr-only')).map(
    (node) => node.textContent?.trim() ?? '',
  );
}

function click(target: Element, init: MouseEventInit = {}): MouseEvent {
  const event = new MouseEvent('click', { bubbles: true, cancelable: true, button: 0, ...init });
  target.dispatchEvent(event);
  return event;
}

async function mountList(props: Partial<OpmFolderList> = {}): Promise<OpmFolderList> {
  return mount('opm-folder-list', { folders: [folderEntry({ name: 'trip' })], ...props });
}

describe('folder thumbnails', () => {
  it('shows lazy, aspect-preserving previews with counts and the same navigation', async () => {
    const entry = folderEntry({ name: 'trip', matches: 2, preview: { id: 42, mtime: 123 } });
    const host = await mountList({
      folders: [entry],
      view: 'thumbnails',
      showMatches: true,
      hrefFor: () => '/browse?source=photos/trip',
    });
    const opened = vi.fn();
    host.addEventListener('folder-open', opened);
    const anchor = row(host, 'trip');
    const image = anchor.querySelector('img');
    expect(image?.getAttribute('src')).toBe('/api/files/42/thumb?size=sm&v=123');
    expect(image?.getAttribute('loading')).toBe('lazy');
    expect(image?.classList.contains('object-contain')).toBe(true);
    expect(badges(anchor)).toEqual(['3', '2 matches']);
    expect(anchor.getAttribute('href')).toBe('/browse?source=photos/trip');
    expect(click(image as HTMLImageElement).defaultPrevented).toBe(true);
    expect(opened).toHaveBeenCalledOnce();
    expect(click(anchor, { ctrlKey: true }).defaultPrevented).toBe(false);
    const menu = vi.fn();
    host.addEventListener('folder-menu', menu);
    anchor.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    expect(menu).toHaveBeenCalledOnce();
    host.view = 'list';
    await host.updateComplete;
    expect(host.querySelector('img')).toBeNull();
    expect(row(host, 'trip').classList.contains('list-row')).toBe(true);
  });

  it('falls back for empty and planned folders, and reports unavailable previews', async () => {
    const host = await mountList({
      view: 'thumbnails',
      folders: [
        folderEntry({ name: 'empty', count: 0, total: 0 }),
        folderEntry({ name: 'planned', overlay: { planned: true } }),
        folderEntry({ name: 'photo', preview: { id: 7, mtime: 12 } }),
      ],
    });
    expect(iconOf(row(host, 'empty'))).toBe('folder');
    expect(iconOf(row(host, 'planned'))).toBe('folder-plus');
    expect(badges(row(host, 'planned'))).toContain('new');
    row(host, 'photo').querySelector('img')?.dispatchEvent(new Event('error'));
    await host.updateComplete;
    expect(row(host, 'photo').querySelector('img')).toBeNull();
    expect(iconOf(row(host, 'photo'))).toBe('image-off');
    expect(srText(row(host, 'photo'))).toContain('Photo preview unavailable');
    host.folders = [folderEntry({ name: 'photo', preview: { id: 7, mtime: 13 } })];
    await host.updateComplete;
    expect(row(host, 'photo').querySelector('img')?.getAttribute('src')).toContain('v=13');
  });

  it('keeps hide-unmatched and plan-state semantics in thumbnail mode', async () => {
    const host = await mountList({
      view: 'thumbnails',
      hideUnmatched: true,
      folders: [
        folderEntry({ name: 'unmatched', matches: 0 }),
        folderEntry({ name: 'deleted', matches: 0, overlay: { will_delete: true } }),
        folderEntry({ name: 'matched', matches: 1 }),
      ],
    });
    expect(names(host)).toEqual(['deleted', 'matched']);
    expect(srText(row(host, 'deleted'))).toContain('will be deleted');
    expect(row(host, 'deleted').classList.contains('border-error')).toBe(true);
  });
});

describe('opm-folder-list structure', () => {
  it('is a named list with one row per folder, in order', async () => {
    const host = await mountList({
      pane: 'comparison',
      folders: [folderEntry({ name: 'b' }), folderEntry({ name: 'a' }), folderEntry({ name: 'c' })],
    });

    const list = host.querySelector('ul');
    expect(list?.classList.contains('list')).toBe(true);
    expect(list?.getAttribute('role')).toBe('list');
    expect(list?.getAttribute('aria-label')).toBe('Comparison folders');
    expect(names(host)).toEqual(['b', 'a', 'c']);
    expect(host.classList.contains('block')).toBe(true);
  });

  it('names the list after the pane', async () => {
    const host = await mountList({ pane: 'source' });

    expect(host.querySelector('ul')?.getAttribute('aria-label')).toBe('Source folders');
  });

  it('makes every row a DaisyUI list row inside an item that skips off-screen work', async () => {
    const host = await mountList();

    const item = host.querySelector('li');
    expect(item?.classList.contains('[content-visibility:auto]')).toBe(true);
    expect(item?.className).toContain('[contain-intrinsic-size:auto_2.1875rem]');
    const anchor = row(host, 'trip');
    expect(anchor.classList.contains('list-row')).toBe(true);
    expect(anchor.classList.contains('hover:bg-base-200')).toBe(true);
  });

  it('draws the focus ring inside the row, where the paint containment cannot clip it', async () => {
    const host = await mountList();

    const anchor = row(host, 'trip');
    expect(anchor.classList.contains('focus-visible:outline-2')).toBe(true);
    expect(anchor.classList.contains('focus-visible:-outline-offset-2')).toBe(true);
    expect(anchor.classList.contains('focus-visible:outline-primary')).toBe(true);
  });

  it('renders nothing for an empty list', async () => {
    const host = await mountList({ folders: [] });

    expect(host.querySelector('ul')).toBeNull();
    expect(host.textContent?.trim()).toBe('');
  });

  it('shows the name in full in its tooltip and as text, whatever it contains', async () => {
    const nasty = '<img src=x onerror=alert(1)> & "quotes"';
    const host = await mountList({
      folders: [
        folderEntry({ name: 'Été 日本語' }),
        folderEntry({ name: '.hidden' }),
        folderEntry({ name: nasty }),
      ],
    });

    const label = (name: string) => row(host, name).querySelector<HTMLElement>('[title]');
    expect(label('Été 日本語')?.textContent).toBe('Été 日本語');
    expect(label('.hidden')?.getAttribute('title')).toBe('.hidden');
    expect(label(nasty)?.textContent).toBe(nasty);
    expect(label(nasty)?.getAttribute('title')).toBe(nasty);
    expect(host.querySelector('img')).toBeNull();
  });

  it('truncates a long name instead of growing the row', async () => {
    const host = await mountList({ folders: [folderEntry({ name: 'x'.repeat(300) })] });

    const name = row(host, 'x'.repeat(300)).querySelector('.list-col-grow');
    expect(name?.classList.contains('truncate')).toBe(true);
    expect(name?.classList.contains('min-w-0')).toBe(true);
  });

  it('keeps a row where it is while the list around it changes', async () => {
    const host = await mountList({
      folders: [folderEntry({ name: 'a' }), folderEntry({ name: 'b' }), folderEntry({ name: 'c' })],
    });
    const b = row(host, 'b');

    host.folders = [folderEntry({ name: 'c' }), folderEntry({ name: 'b', count: 9, total: 9 })];
    await host.updateComplete;

    expect(names(host)).toEqual(['c', 'b']);
    expect(row(host, 'b')).toBe(b);
    expect(badges(b)).toEqual(['9']);
  });
});

describe('opm-folder-list icons', () => {
  it.each([
    ['a folder', folderEntry({ name: 'trip' }), 'folder'],
    ['a volume', folderEntry({ name: 'photos', path: '' }), 'hard-drive'],
    ['a folder the plan creates', folderEntry({ overlay: { planned: true } }), 'folder-plus'],
    ['a folder the plan deletes', folderEntry({ overlay: { will_delete: true } }), 'folder-x'],
  ])('uses the icon of %s', async (_name, entry, icon) => {
    const host = await mountList({ folders: [entry] });

    expect(iconOf(anchors(host)[0] as HTMLElement)).toBe(icon);
  });
});

describe('opm-folder-list counts', () => {
  it('shows one number when the filter hides nothing', async () => {
    const host = await mountList({ folders: [folderEntry({ name: 'a', count: 40, total: 40 })] });

    const anchor = row(host, 'a');
    expect(badges(anchor)).toEqual(['40']);
    expect(srText(anchor)).toEqual(['40 items']);
  });

  it('shows how many match out of how many when the filter hides some', async () => {
    const host = await mountList({ folders: [folderEntry({ name: 'a', count: 12, total: 40 })] });

    const anchor = row(host, 'a');
    expect(badges(anchor)).toEqual(['12 / 40']);
    expect(srText(anchor)).toEqual(['12 of 40 items']);
    expect(anchor.querySelector('.badge')?.getAttribute('aria-hidden')).toBe('true');
    expect(anchor.querySelector('.badge')?.classList.contains('badge-ghost')).toBe(true);
  });

  it('says "1 item" for one', async () => {
    const host = await mountList({ folders: [folderEntry({ name: 'a', count: 1, total: 1 })] });

    expect(srText(row(host, 'a'))).toEqual(['1 item']);
  });

  it('shows the count of an emptied folder, with its total', async () => {
    const host = await mountList({ folders: [folderEntry({ name: 'a', count: 0, total: 25 })] });

    expect(badges(row(host, 'a'))).toEqual(['0 / 25']);
    expect(srText(row(host, 'a'))).toEqual(['0 of 25 items']);
  });

  it('formats big numbers with the separators of the locale', async () => {
    const host = await mountList({
      folders: [folderEntry({ name: 'a', count: 1234, total: 1_234_567 })],
    });

    expect(badges(row(host, 'a'))).toEqual([`${formatCount(1234)} / ${formatCount(1_234_567)}`]);
  });
});

describe('opm-folder-list matches', () => {
  const folders = [
    folderEntry({ name: 'some', matches: 3 }),
    folderEntry({ name: 'one', matches: 1 }),
    folderEntry({ name: 'none', matches: 0 }),
    folderEntry({ name: 'unknown', matches: null }),
  ];

  it('is quiet by default', async () => {
    const host = await mountList({ folders });

    for (const anchor of anchors(host)) expect(badges(anchor)).toEqual(['3']);
  });

  it('shows the matches of the folders that have a number', async () => {
    const host = await mountList({ folders, showMatches: true });

    expect(badges(row(host, 'some'))).toEqual(['3', '3 matches']);
    expect(badges(row(host, 'one'))).toEqual(['3', '1 match']);
    expect(badges(row(host, 'unknown'))).toEqual(['3']);
  });

  it('colours a match green and zero matches ghost grey', async () => {
    const host = await mountList({ folders, showMatches: true });

    const matchBadge = (name: string) => row(host, name).querySelectorAll('.badge')[1];
    expect(matchBadge('some')?.classList.contains('badge-success')).toBe(true);
    expect(matchBadge('none')?.textContent?.trim()).toBe('0 matches');
    expect(matchBadge('none')?.classList.contains('badge-ghost')).toBe(true);
    expect(matchBadge('none')?.classList.contains('badge-success')).toBe(false);
  });

  it('follows the property', async () => {
    const host = await mountList({ folders, showMatches: true });

    host.showMatches = false;
    await host.updateComplete;

    expect(badges(row(host, 'some'))).toEqual(['3']);
  });

  it('reads the show-matches attribute', async () => {
    const host = document.createElement('opm-folder-list');
    host.setAttribute('show-matches', '');

    await mount(host, { folders });

    expect(badges(row(host, 'some'))).toEqual(['3', '3 matches']);
  });
});

describe('opm-folder-list plan overlay', () => {
  it('marks a folder the plan creates with a dashed border and the text "new"', async () => {
    const host = await mountList({
      folders: [folderEntry({ name: 'sorted', count: 4, total: 4, overlay: { planned: true } })],
    });

    const anchor = row(host, 'sorted');
    expect(anchor.classList.contains('border-dashed')).toBe(true);
    expect(anchor.classList.contains('border-info')).toBe(true);
    expect(badges(anchor)).toEqual(['new', '4']);
    expect(anchor.querySelector('.badge-info')?.getAttribute('title')).toBe('Will be created');
  });

  it('marks a folder the plan deletes with a red border and icon, and says so in words', async () => {
    const host = await mountList({
      folders: [folderEntry({ name: 'old', count: 0, total: 0, overlay: { will_delete: true } })],
    });

    const anchor = row(host, 'old');
    expect(anchor.classList.contains('border-error')).toBe(true);
    expect(anchor.classList.contains('text-base-content')).toBe(true);
    expect(anchor.classList.contains('text-error')).toBe(false);
    expect(anchor.querySelector('opm-icon')?.classList.contains('text-error')).toBe(true);
    expect(srText(anchor)).toContain('will be deleted');
  });

  it('gives a plain folder a transparent border, so the rows stay the same height', async () => {
    const host = await mountList();

    const anchor = row(host, 'trip');
    expect(anchor.classList.contains('border')).toBe(true);
    expect(anchor.classList.contains('border-transparent')).toBe(true);
    expect(anchor.classList.contains('border-dashed')).toBe(false);
    expect(anchor.classList.contains('border-error')).toBe(false);
    expect(srText(anchor)).not.toContain('will be deleted');
    expect(badges(anchor)).not.toContain('new');
  });
});

describe('opm-folder-list grey and hide', () => {
  const folders = [
    folderEntry({ name: 'full', count: 5, total: 5 }),
    folderEntry({ name: 'emptied', count: 0, total: 5 }),
    folderEntry({ name: 'nomatch', count: 5, total: 5, matches: 0 }),
    folderEntry({ name: 'created', count: 0, total: 0, overlay: { planned: true } }),
    folderEntry({ name: 'removed', count: 0, total: 0, overlay: { will_delete: true } }),
  ];

  it('mutes unmatched folders without lowering text contrast and keeps them clickable', async () => {
    const host = await mountList({ folders });

    expect(names(host)).toEqual(['full', 'emptied', 'nomatch', 'created', 'removed']);
    const muted = (name: string) => row(host, name).classList.contains('text-base-content/70');
    expect(muted('full')).toBe(false);
    expect(muted('emptied')).toBe(true);
    expect(muted('nomatch')).toBe(true);
    expect(row(host, 'emptied').classList.contains('opacity-50')).toBe(false);
    expect(row(host, 'emptied').getAttribute('href')).toBe('#');
  });

  it('hides them when asked to, except the folders of the plan, which are greyed', async () => {
    const host = await mountList({ folders, hideUnmatched: true });

    expect(names(host)).toEqual(['full', 'created', 'removed']);
    expect(row(host, 'created').classList.contains('text-base-content/70')).toBe(true);
    expect(row(host, 'removed').querySelector('opm-icon')?.classList.contains('text-error')).toBe(
      true,
    );
    expect(row(host, 'removed').classList.contains('text-base-content/70')).toBe(false);
    expect(row(host, 'full').classList.contains('text-base-content/70')).toBe(false);
  });

  it('follows hideUnmatched', async () => {
    const host = await mountList({ folders, hideUnmatched: true });

    host.hideUnmatched = false;
    await host.updateComplete;

    expect(names(host)).toHaveLength(5);
  });

  it('renders nothing when every folder is hidden', async () => {
    const host = await mountList({ folders: [folders[1] as never], hideUnmatched: true });

    expect(host.querySelector('ul')).toBeNull();
  });

  it('does not show skeletons for folders that are only hidden', async () => {
    const host = await mountList({
      folders: [folders[1] as never],
      hideUnmatched: true,
      loading: true,
    });

    expect(host.querySelector('[role="status"]')).toBeNull();
  });
});

describe('opm-folder-list links', () => {
  it('links each row where hrefFor says, so it can open in a new tab', async () => {
    const hrefFor = vi.fn((entry: { name: string }) => `/browse?src=photo/${entry.name}`);
    const host = await mountList({
      folders: [folderEntry({ name: 'a' }), folderEntry({ name: 'b' })],
      hrefFor,
    });

    expect(row(host, 'a').getAttribute('href')).toBe('/browse?src=photo/a');
    expect(row(host, 'b').getAttribute('href')).toBe('/browse?src=photo/b');
    expect(hrefFor.mock.calls.map(([entry]) => entry.name)).toEqual(['a', 'b']);
  });

  it('links to # without hrefFor', async () => {
    const host = await mountList();

    expect(row(host, 'trip').getAttribute('href')).toBe('#');
  });

  it('follows a new hrefFor', async () => {
    const host = await mountList({ hrefFor: () => '/one' });

    host.hrefFor = () => '/two';
    await host.updateComplete;

    expect(row(host, 'trip').getAttribute('href')).toBe('/two');
  });
});

describe('opm-folder-list opening', () => {
  it('emits folder-open with the entry on a plain left click, and keeps the page from loading', async () => {
    const entry = folderEntry({ name: 'trip' });
    const host = await mountList({
      folders: [folderEntry({ name: 'x' }), entry],
      hrefFor: () => '/a',
    });
    const seen = vi.fn();
    document.body.addEventListener('folder-open', seen);

    const event = click(row(host, 'trip'));

    document.body.removeEventListener('folder-open', seen);
    expect(event.defaultPrevented).toBe(true);
    expect(seen).toHaveBeenCalledTimes(1);
    const opened = seen.mock.calls[0]?.[0] as FolderOpenEvent;
    expect(opened.detail.entry).toBe(entry);
    expect(opened.target).toBe(host);
  });

  it('opens from a click on any part of the row', async () => {
    const host = await mountList();
    const seen = vi.fn();
    host.addEventListener('folder-open', seen);

    click(row(host, 'trip').querySelector('.list-col-grow') as Element);
    click(row(host, 'trip').querySelector('.badge') as Element);
    click(row(host, 'trip').querySelector('opm-icon') as Element);

    expect(seen).toHaveBeenCalledTimes(3);
  });

  it.each([
    ['a middle click', { button: 1 }],
    ['a right click', { button: 2 }],
    ['ctrl-click', { ctrlKey: true }],
    ['cmd-click', { metaKey: true }],
    ['shift-click', { shiftKey: true }],
    ['alt-click', { altKey: true }],
  ])('leaves %s to the browser', async (_name, init) => {
    const host = await mountList();
    const seen = vi.fn();
    host.addEventListener('folder-open', seen);

    const event = click(row(host, 'trip'), init);

    expect(seen).not.toHaveBeenCalled();
    expect(event.defaultPrevented).toBe(false);
  });

  it('leaves a click somebody else already handled alone', async () => {
    const host = await mountList();
    const seen = vi.fn();
    host.addEventListener('folder-open', seen);
    const anchor = row(host, 'trip');
    anchor.addEventListener('click', (event) => event.preventDefault());

    click(anchor);

    expect(seen).not.toHaveBeenCalled();
  });

  it('ignores a click between the rows', async () => {
    const host = await mountList();
    const seen = vi.fn();
    host.addEventListener('folder-open', seen);

    click(host.querySelector('ul') as Element);
    click(host.querySelector('li') as Element);

    expect(seen).not.toHaveBeenCalled();
  });

  it('opens the right entry after the list changed', async () => {
    const host = await mountList({ folders: [folderEntry({ name: 'a' })] });
    const second = folderEntry({ name: 'b', count: 7, total: 7 });
    host.folders = [second, folderEntry({ name: 'a' })];
    await host.updateComplete;
    const seen = vi.fn();
    host.addEventListener('folder-open', seen);

    click(row(host, 'b'));

    const [event] = seen.mock.calls[0] as [FolderOpenEvent];
    expect(event.detail.entry).toBe(second);
  });

  it('opens a greyed folder too', async () => {
    const host = await mountList({ folders: [folderEntry({ name: 'emptied', count: 0 })] });
    const seen = vi.fn();
    host.addEventListener('folder-open', seen);

    click(row(host, 'emptied'));

    expect(seen).toHaveBeenCalledTimes(1);
  });

  it('is one tab stop per row: an anchor with an address and nothing focusable inside', async () => {
    const host = await mountList({
      folders: [folderEntry({ name: 'a' }), folderEntry({ name: 'b', overlay: { planned: true } })],
      showMatches: true,
    });

    for (const anchor of anchors(host)) {
      expect(anchor.tagName).toBe('A');
      expect(anchor.hasAttribute('href')).toBe(true);
      expect(anchor.hasAttribute('tabindex')).toBe(false);
      expect(anchor.querySelectorAll('a, button, input, select, [tabindex]')).toHaveLength(0);
    }
    expect(host.querySelectorAll('li')).toHaveLength(2);
  });
});

describe('opm-folder-list loading', () => {
  it('shows skeleton rows while the first answer is awaited', async () => {
    const host = await mountList({ folders: [], loading: true });

    const status = host.querySelector('[role="status"]');
    expect(status?.querySelector('.sr-only')?.textContent).toBe('Loading folders');
    const skeletons = host.querySelectorAll('.skeleton');
    expect(skeletons.length).toBeGreaterThan(0);
    expect(skeletons[0]?.closest('[aria-hidden="true"]')).not.toBeNull();
    expect(host.querySelector('ul')).toBeNull();
  });

  it('keeps the rows on screen while a refresh is in flight', async () => {
    const host = await mountList({ loading: true });

    expect(host.querySelector('[role="status"]')).toBeNull();
    expect(host.querySelector('.skeleton')).toBeNull();
    expect(names(host)).toEqual(['trip']);
  });

  it('swaps the skeletons for the rows when the folders arrive', async () => {
    const host = await mountList({ folders: [], loading: true });

    host.folders = [folderEntry({ name: 'a' })];
    host.loading = false;
    await host.updateComplete;

    expect(host.querySelector('.skeleton')).toBeNull();
    expect(names(host)).toEqual(['a']);
  });

  it('shows nothing for an empty list that is not loading', async () => {
    const host = await mountList({ folders: [], loading: false });

    expect(host.querySelector('.skeleton')).toBeNull();
    expect(host.querySelector('[role="status"]')).toBeNull();
  });
});

describe('opm-folder-list at scale', () => {
  it('renders a few thousand rows quickly, every one of them', async () => {
    const folders = Array.from({ length: 3000 }, (_, index) =>
      folderEntry({ name: `folder ${index}`, count: index, total: index + 5 }),
    );

    const started = performance.now();
    const host = await mountList({
      folders,
      hrefFor: (entry) => `/browse?src=photo/${entry.name}`,
    });
    const elapsed = performance.now() - started;

    expect(anchors(host)).toHaveLength(3000);
    expect(elapsed).toBeLessThan(3000);
  });
});

describe('the folder menu', () => {
  function rightClick(target: Element): MouseEvent {
    const event = new MouseEvent('contextmenu', {
      bubbles: true,
      cancelable: true,
      clientX: 10,
      clientY: 20,
    });
    target.dispatchEvent(event);
    return event;
  }

  it('asks for the menu of a folder on a right click', async () => {
    const host = await mountList();
    const seen: FolderMenuEvent[] = [];
    host.addEventListener('folder-menu', (event) => seen.push(event));

    const event = rightClick(row(host, 'trip'));

    expect(event.defaultPrevented).toBe(true);
    expect(seen.map((item) => [item.detail.entry.path, item.detail.x, item.detail.y])).toEqual([
      ['trip', 10, 20],
    ]);
  });

  it('keeps the browser menu on a volume and on a planned folder', async () => {
    const host = await mountList({
      folders: [
        folderEntry({ name: 'photos', path: '' }),
        folderEntry({ name: 'new', overlay: { planned: true } }),
      ],
    });
    const seen = vi.fn();
    host.addEventListener('folder-menu', seen);

    for (const name of ['photos', 'new'])
      expect(rightClick(row(host, name)).defaultPrevented).toBe(false);
    expect(seen).not.toHaveBeenCalled();
  });
});
