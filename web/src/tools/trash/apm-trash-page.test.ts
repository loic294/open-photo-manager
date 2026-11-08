import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { api, type RequestOptions } from '@/api/client';
import { ApiError } from '@/api/errors';
import type { TaskDto } from '@/api/generated/TaskDto';
import type { TrashBatchDto } from '@/api/generated/TrashBatchDto';
import type { TrashItemDto } from '@/api/generated/TrashItemDto';
import type { TrashSummary } from '@/api/generated/TrashSummary';
import { events, SERVER_RETRY_MS } from '@/api/sse';
import { toast } from '@/components/toast';
import { clearToasts, toasts } from '@/stores/toasts';
import { pressConfirm, shownConfirm } from '@/test/confirm';
import { FakeEventSource } from '@/test/fake-event-source';
import { mount } from '@/test/helpers';
import { makeTask, taskEvent } from '@/test/task-fixtures';
import {
  makeTrashBatch,
  makeTrashItem,
  makeTrashSummary,
  makeTrashVolume,
  TRASHED_AT,
} from '@/test/trash-fixtures';
import {
  type ApmTrashPage,
  PAGE_SIZE,
  REFRESH_DEBOUNCE_MS,
  SEARCH_DEBOUNCE_MS,
} from '@/tools/trash/apm-trash-page';
import { EM_DASH, formatFileTime } from '@/util/format';
import '@/tools/trash/apm-trash-page';

const ELLIPSIS = '\u2026';
const EMPTY_TRASH = `Empty trash${ELLIPSIS}`;

/** Three days after the fixtures were trashed, so their age reads "3 days ago". */
const NOW_MS = (TRASHED_AT + 3 * 86_400) * 1000;

const photos = makeTrashVolume({ volume_id: 1, name: 'photos', items: 10, bytes: 10_240 });
const archive = makeTrashVolume({ volume_id: 2, name: 'archive', items: 5, bytes: 5_120 });

const twoVolumes = () => makeTrashSummary({ volumes: [photos, archive] });

const defaultItems = (): TrashItemDto[] => [
  makeTrashItem({ id: 1, name: 'IMG_0001.jpg', size: 2_048 }),
  makeTrashItem({ id: 2, name: 'IMG_0002.jpg', size: 4_096 }),
  makeTrashItem({
    id: 3,
    name: 'Trip',
    dir: '2024',
    is_dir: true,
    size: 1_048_576,
    reason: 'replaced',
  }),
];

const manyItems = (count: number): TrashItemDto[] =>
  Array.from({ length: count }, (_, index) =>
    makeTrashItem({ id: index + 1, name: `IMG_${index}.jpg` }),
  );

const manyBatches = (count: number): TrashBatchDto[] =>
  Array.from({ length: count }, (_, index) =>
    makeTrashBatch({ batch: `batch-${index}`, items: 1, bytes: 1_024 }),
  );

// ---- a fake server -------------------------------------------------------------------------------

interface Sent {
  method: 'GET' | 'POST';
  path: string;
  /** What a GET asked for: the complete query, nulls included (the client skips them on the wire). */
  query: Record<string, unknown>;
  body: unknown;
  signal: AbortSignal | undefined;
}

interface Held {
  sent: Sent;
  answer(value: unknown): void;
  fail(error: unknown): void;
}

interface Server {
  summary: TrashSummary;
  batches: TrashBatchDto[];
  items: TrashItemDto[];
  sent: Sent[];
  /** Replaces the answer of one route (`"GET /api/trash"`): to fail it or to hold it. */
  routes: Map<string, (sent: Sent) => unknown>;
}

let server: Server;
let page: ApmTrashPage;

function pageOf<T>(all: readonly T[], query: Record<string, unknown>): T[] {
  const offset = Number(query.offset ?? 0);
  const limit = Number(query.limit ?? PAGE_SIZE);
  return all.slice(offset, offset + limit);
}

/** What the server answers with the state it has now. */
function defaultReply(sent: Sent): unknown {
  switch (`${sent.method} ${sent.path}`) {
    case 'GET /api/trash/batches':
      return {
        batches: pageOf(server.batches, sent.query),
        total: server.batches.length,
        summary: server.summary,
      };
    case 'GET /api/trash':
      return {
        items: pageOf(server.items, sent.query),
        total: server.items.length,
        summary: server.summary,
      };
    case 'POST /api/trash/restore':
      return makeTask({
        id: 7,
        kind: 'fs_move',
        state: 'queued',
        title: 'Restoring 3 items from the trash',
      });
    case 'POST /api/trash/empty':
      return makeTask({ id: 8, kind: 'trash_empty', state: 'queued', title: 'Emptying the trash' });
    case 'POST /api/trash/sync':
      return makeTask({ id: 9, kind: 'fs_trash', state: 'queued', title: 'Checking the trash' });
    default:
      throw new Error(`The fake server has no answer for ${sent.method} ${sent.path}`);
  }
}

function installServer(): void {
  server = {
    summary: makeTrashSummary(),
    batches: [makeTrashBatch()],
    items: defaultItems(),
    sent: [],
    routes: new Map(),
  };
  const handle = (method: Sent['method'], path: string, options: RequestOptions = {}) => {
    const sent: Sent = {
      method,
      path,
      query: { ...(options.query ?? {}) },
      body: options.body,
      signal: options.signal,
    };
    server.sent.push(sent);
    const route = server.routes.get(`${method} ${path}`);
    return new Promise<unknown>((resolve) => resolve(route ? route(sent) : defaultReply(sent)));
  };
  vi.spyOn(api, 'get').mockImplementation(((path: string, options?: RequestOptions) =>
    handle('GET', path, options)) as typeof api.get);
  vi.spyOn(api, 'post').mockImplementation(((path: string, options?: RequestOptions) =>
    handle('POST', path, options)) as typeof api.post);
}

const requests = (route: string): Sent[] =>
  server.sent.filter((sent) => `${sent.method} ${sent.path}` === route);

function last(route: string): Sent {
  const found = requests(route).at(-1);
  if (!found) throw new Error(`No ${route} request was sent`);
  return found;
}

const BATCHES = 'GET /api/trash/batches';
const ITEMS = 'GET /api/trash';
const RESTORE = 'POST /api/trash/restore';
const EMPTY = 'POST /api/trash/empty';
const SYNC = 'POST /api/trash/sync';

/** Makes every request of a route wait until the test answers or fails it. */
function hold(route: string, options: { honourAbort?: boolean } = {}): Held[] {
  const { honourAbort = true } = options;
  const held: Held[] = [];
  server.routes.set(
    route,
    (sent) =>
      new Promise((resolve, reject) => {
        if (honourAbort) {
          sent.signal?.addEventListener('abort', () =>
            reject(new DOMException('Aborted', 'AbortError')),
          );
        }
        held.push({ sent, answer: resolve, fail: reject });
      }),
  );
  return held;
}

function refuse(route: string, error: unknown): void {
  server.routes.set(route, () => Promise.reject(error));
}

const serverError = () =>
  new ApiError({ status: 500, code: 'internal', message: 'The server is down' });

// ---- the DOM ---------------------------------------------------------------------------------------

const text = (element: Element | null | undefined) =>
  element?.textContent?.replace(/\s+/g, ' ').trim() ?? '';

function required<T extends Element>(root: ParentNode, selector: string): T {
  const found = root.querySelector<T>(selector);
  if (!found) throw new Error(`Nothing matches "${selector}"`);
  return found;
}

function button(scope: ParentNode, label: string): HTMLButtonElement {
  const found = [...scope.querySelectorAll<HTMLButtonElement>('button')].find(
    (candidate) => text(candidate) === label,
  );
  if (!found) throw new Error(`No "${label}" button`);
  return found;
}

function labelled<T extends HTMLElement>(scope: ParentNode, label: string): T {
  return required<T>(scope, `[aria-label="${label}"]`);
}

const rows = (scope: ParentNode = page) => [...scope.querySelectorAll('tbody tr')];
const cells = (row: Element, count: number) =>
  [...row.querySelectorAll('td')].slice(0, count).map(text);
const messages = () => toasts.value.map((toast) => [toast.kind, toast.message]);

const tab = (name: 'deletions' | 'items') =>
  required<HTMLButtonElement>(page, `#trash-tab-${name}`);
const openTab = () =>
  tab('items').getAttribute('aria-selected') === 'true' ? 'items' : 'deletions';

function pickVolume(value: string): void {
  const select = labelled<HTMLSelectElement>(page, 'Volume');
  select.value = value;
  select.dispatchEvent(new Event('change', { bubbles: true }));
}

function type(value: string): void {
  const input = labelled<HTMLInputElement>(page, 'Search the trash');
  input.value = value;
  input.dispatchEvent(new Event('input', { bubbles: true }));
}

function press(target: Element, key: string, modifiers: KeyboardEventInit = {}): void {
  target.dispatchEvent(
    new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...modifiers }),
  );
}

const sortOf = (label: string) => button(page, label).closest('th')?.getAttribute('aria-sort');

// ---- time and rendering ------------------------------------------------------------------------------

// Lets the page and the components it draws (which render on their own) catch up.
async function rendered(): Promise<void> {
  for (let pass = 0; pass < 3; pass++) {
    await page.updateComplete;
    const parts = page.querySelectorAll<HTMLElement & { updateComplete: Promise<unknown> }>(
      'apm-trash-deletions, apm-trash-items, apm-select',
    );
    await Promise.all([...parts].map((part) => part.updateComplete));
  }
}

async function flush(): Promise<void> {
  await vi.advanceTimersByTimeAsync(0);
  await rendered();
}

async function advance(ms: number): Promise<void> {
  await vi.advanceTimersByTimeAsync(ms);
  await flush();
}

async function click(element: HTMLElement): Promise<void> {
  element.click();
  await flush();
}

async function mountPage(): Promise<ApmTrashPage> {
  page = await mount('apm-trash-page');
  await flush();
  return page;
}

async function openItems(): Promise<ApmTrashPage> {
  await mountPage();
  await click(tab('items'));
  return page;
}

async function answerHeld(held: Held): Promise<void> {
  held.answer(defaultReply(held.sent));
  await flush();
}

/** The window is shown again: the page refetches once its pause has passed. */
async function refetch(): Promise<void> {
  window.dispatchEvent(new Event('focus'));
  await advance(REFRESH_DEBOUNCE_MS);
}

async function confirmRestoreAll(label = 'Restore 3 items'): Promise<void> {
  await click(button(page, 'Restore all'));
  await pressConfirm(label);
  await flush();
}

async function confirmEmpty(label = 'Empty trash'): Promise<void> {
  await click(button(page, EMPTY_TRASH));
  await pressConfirm(label);
  await flush();
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(NOW_MS);
  clearToasts();
  installServer();
});

afterEach(() => {
  clearToasts();
  events.stop();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  FakeEventSource.reset();
});

describe('apm-trash-page opening', () => {
  it('is a block host with the heading, the totals and the volumes that hold trash', async () => {
    server.summary = twoVolumes();
    await mountPage();
    expect(page.classList.contains('block')).toBe(true);
    expect(text(page.querySelector('h2'))).toBe('Trash');
    expect([...page.querySelectorAll('.stat')].map(text)).toEqual(['Items 15', 'Total size 15 KB']);
    expect([...page.querySelectorAll('ul[aria-label="Volumes with trash"] li')].map(text)).toEqual([
      'photos \u00b7 10 KB',
      'archive \u00b7 5 KB',
    ]);
  });

  it('loads the deletions and nothing else when it opens', async () => {
    await mountPage();
    expect(server.sent.map((sent) => `${sent.method} ${sent.path}`)).toEqual([BATCHES]);
    expect(last(BATCHES).query).toEqual({ volume_id: null, offset: 0, limit: PAGE_SIZE });
  });

  it('shows placeholders and no totals until the first answer', async () => {
    const held = hold(BATCHES);
    await mountPage();
    expect(page.querySelector('[role="status"][aria-label="Loading deletions"]')).not.toBeNull();
    expect([...page.querySelectorAll('.stat-value')].map(text)).toEqual([EM_DASH, EM_DASH]);
    expect(button(page, EMPTY_TRASH).disabled).toBe(true);
    expect(button(page, EMPTY_TRASH).title).toBe('The trash is still loading.');

    await answerHeld(held[0] as Held);
    expect(page.querySelector('[aria-label="Loading deletions"]')).toBeNull();
    expect(rows()).toHaveLength(1);
    expect([...page.querySelectorAll('.stat-value')].map(text)).toEqual(['10', '10 KB']);
    expect(button(page, EMPTY_TRASH).disabled).toBe(false);
  });

  it('lists one row per deletion, in the order the server sent them', async () => {
    server.summary = twoVolumes();
    server.batches = [
      makeTrashBatch(),
      makeTrashBatch({
        volume_id: 2,
        volume: 'archive',
        batch: '20241231-090000-t3',
        task_id: 3,
        trashed_at: TRASHED_AT - 86_400,
        items: 2,
        bytes: 2_048,
        reasons: ['replaced'],
      }),
    ];
    await mountPage();
    expect(rows().map((row) => cells(row, 5))).toEqual([
      ['3 days ago', 'photos', '3', '6 KB', 'Deleted'],
      ['4 days ago', 'archive', '2', '2 KB', 'Replaced'],
    ]);
  });

  it.each([
    [null, 'Items stay in the trash until you empty it.'],
    [30, 'Items are deleted automatically 30 days after they were trashed.'],
  ])('states the retention rule (%s days) with a link to the settings', async (days, sentence) => {
    server.summary = makeTrashSummary({ retention_days: days });
    await mountPage();
    const note = required(page, '.alert-info');
    expect(text(note)).toBe(`${sentence} Change in Settings`);
    expect(required(note, 'a').getAttribute('href')).toMatch(/^\/settings\?section=general/);
  });
});

describe('apm-trash-page tabs', () => {
  it('is a tablist whose open tab owns the only panel', async () => {
    await mountPage();
    const list = required(page, '[role="tablist"]');
    expect(list.getAttribute('aria-label')).toBe('Trash views');
    const tabs = [...list.querySelectorAll('[role="tab"]')];
    expect(tabs.map(text)).toEqual(['Deletions', 'Items']);
    expect(tabs.map((entry) => entry.getAttribute('aria-selected'))).toEqual(['true', 'false']);
    expect(tabs.map((entry) => entry.getAttribute('tabindex'))).toEqual(['0', '-1']);
    const panels = page.querySelectorAll('[role="tabpanel"]');
    expect(panels).toHaveLength(1);
    expect(panels[0]?.getAttribute('aria-labelledby')).toBe('trash-tab-deletions');
    expect(tabs[0]?.getAttribute('aria-controls')).toBe(panels[0]?.id);

    await click(tab('items'));
    expect(page.querySelectorAll('[role="tabpanel"]')).toHaveLength(1);
    expect(page.querySelector('[role="tabpanel"]')?.getAttribute('aria-labelledby')).toBe(
      'trash-tab-items',
    );
    expect(tab('items').getAttribute('tabindex')).toBe('0');
    expect(tab('deletions').getAttribute('tabindex')).toBe('-1');
  });

  it('loads the items the first time their tab opens, and only then', async () => {
    await mountPage();
    expect(requests(ITEMS)).toHaveLength(0);
    await click(tab('items'));
    expect(requests(ITEMS)).toHaveLength(1);
    expect(last(ITEMS).query).toEqual({
      volume_id: null,
      batch: null,
      q: null,
      sort: 'trashed_at',
      order: null,
      offset: 0,
      limit: PAGE_SIZE,
    });
    expect(rows()).toHaveLength(3);
  });

  it('does not reload a tab that is opened again', async () => {
    await openItems();
    await click(tab('deletions'));
    await click(tab('items'));
    expect(requests(ITEMS)).toHaveLength(1);
    expect(requests(BATCHES)).toHaveLength(1);
  });

  it('moves between the tabs with the arrow keys, Home and End', async () => {
    await mountPage();
    press(tab('deletions'), 'ArrowRight');
    await flush();
    expect(openTab()).toBe('items');
    expect(document.activeElement).toBe(tab('items'));

    press(tab('items'), 'ArrowRight');
    await flush();
    expect(openTab()).toBe('deletions');

    press(tab('deletions'), 'ArrowLeft');
    await flush();
    expect(openTab()).toBe('items');

    press(tab('items'), 'Home');
    await flush();
    expect(openTab()).toBe('deletions');

    press(tab('deletions'), 'End');
    await flush();
    expect(openTab()).toBe('items');
  });

  it('leaves the arrow keys alone when a modifier is held', async () => {
    await mountPage();
    press(tab('deletions'), 'ArrowRight', { ctrlKey: true });
    await flush();
    expect(openTab()).toBe('deletions');
  });
});

describe('apm-trash-page items', () => {
  it('asks for the newest first and says so', async () => {
    await openItems();
    expect(sortOf('Deleted')).toBe('descending');
    expect(sortOf('Name')).toBe('none');
  });

  it('sorts a column in its natural direction first, then flips it', async () => {
    await openItems();
    await click(button(page, 'Size'));
    expect(last(ITEMS).query).toMatchObject({ sort: 'size', order: null, offset: 0 });
    expect(sortOf('Size')).toBe('descending');

    await click(button(page, 'Size'));
    expect(last(ITEMS).query).toMatchObject({ sort: 'size', order: 'asc' });
    expect(sortOf('Size')).toBe('ascending');

    await click(button(page, 'Size'));
    expect(last(ITEMS).query).toMatchObject({ sort: 'size', order: 'desc' });

    await click(button(page, 'Name'));
    expect(last(ITEMS).query).toMatchObject({ sort: 'name', order: null });
    expect(sortOf('Name')).toBe('ascending');
    expect(sortOf('Size')).toBe('none');
  });

  it('flips the order of the sort it starts with', async () => {
    await openItems();
    await click(button(page, 'Deleted'));
    expect(last(ITEMS).query).toMatchObject({ sort: 'trashed_at', order: 'asc' });
    expect(sortOf('Deleted')).toBe('ascending');
  });

  it('waits for the typing to rest, then searches for the trimmed text once', async () => {
    await openItems();
    type('tr');
    await advance(SEARCH_DEBOUNCE_MS - 1);
    type('  trip  ');
    await advance(SEARCH_DEBOUNCE_MS - 1);
    expect(requests(ITEMS)).toHaveLength(1);
    await advance(1);
    expect(requests(ITEMS)).toHaveLength(2);
    expect(last(ITEMS).query).toMatchObject({ q: 'trip', offset: 0 });
  });

  it('does not ask again when the trimmed text did not change', async () => {
    await openItems();
    type('trip');
    await advance(SEARCH_DEBOUNCE_MS);
    expect(requests(ITEMS)).toHaveLength(2);
    type('trip ');
    await advance(SEARCH_DEBOUNCE_MS);
    expect(requests(ITEMS)).toHaveLength(2);
  });

  it('asks for everything again when the box is emptied', async () => {
    await openItems();
    type('trip');
    await advance(SEARCH_DEBOUNCE_MS);
    type('');
    await advance(SEARCH_DEBOUNCE_MS);
    expect(last(ITEMS).query).toMatchObject({ q: null });
  });

  it('goes back to the first page when the sort or the search changes', async () => {
    server.items = manyItems(120);
    await openItems();
    await click(labelled(page, 'Next page'));
    expect(last(ITEMS).query).toMatchObject({ offset: PAGE_SIZE });

    await click(button(page, 'Size'));
    expect(last(ITEMS).query).toMatchObject({ offset: 0, sort: 'size' });

    await click(labelled(page, 'Next page'));
    type('img');
    await advance(SEARCH_DEBOUNCE_MS);
    expect(last(ITEMS).query).toMatchObject({ offset: 0, q: 'img' });
  });

  it('restores the selected items without asking, then clears the selection', async () => {
    await openItems();
    await click(labelled(page, 'Select IMG_0001.jpg'));
    await click(labelled(page, 'Select Trip'));
    await click(button(page, 'Restore selected (2)'));

    expect(requests(RESTORE)).toHaveLength(1);
    expect(last(RESTORE).body).toEqual({
      target: { kind: 'ids', ids: [1, 3] },
      conflict_policy: null,
    });
    expect(button(page, 'Restore selected (0)').disabled).toBe(true);
    expect(messages()).toEqual([['info', 'Restoring 3 items from the trash']]);
  });

  it('restores one item from its row', async () => {
    await openItems();
    await click(button(rows()[1] as Element, 'Restore'));
    expect(last(RESTORE).body).toEqual({
      target: { kind: 'ids', ids: [2] },
      conflict_policy: null,
    });
  });

  it('selects the whole page except what is on a volume that cannot be changed', async () => {
    server.summary = makeTrashSummary({ volumes: [photos, { ...archive, online: false }] });
    server.items = [
      makeTrashItem({ id: 1, name: 'a.jpg' }),
      makeTrashItem({ id: 2, name: 'b.jpg', volume_id: 2, volume: 'archive' }),
      makeTrashItem({ id: 3, name: 'c.jpg' }),
    ];
    await openItems();
    await click(labelled(page, 'Select all items on this page'));
    expect(labelled<HTMLInputElement>(page, 'Select b.jpg').disabled).toBe(true);
    expect(labelled<HTMLInputElement>(page, 'Select b.jpg').checked).toBe(false);

    await click(button(page, 'Restore selected (2)'));
    expect(last(RESTORE).body).toEqual({
      target: { kind: 'ids', ids: [1, 3] },
      conflict_policy: null,
    });
  });

  it('empties the selection with the header checkbox', async () => {
    await openItems();
    await click(labelled(page, 'Select all items on this page'));
    expect(button(page, 'Restore selected (3)').disabled).toBe(false);
    await click(labelled(page, 'Select all items on this page'));
    expect(button(page, 'Restore selected (0)').disabled).toBe(true);
  });

  it('clears the selection when the page changes', async () => {
    server.items = manyItems(120);
    await openItems();
    await click(labelled(page, 'Select IMG_0.jpg'));
    expect(button(page, 'Restore selected (1)').disabled).toBe(false);
    await click(labelled(page, 'Next page'));
    expect(button(page, 'Restore selected (0)').disabled).toBe(true);
  });

  it('steps through the pages', async () => {
    server.items = manyItems(120);
    await openItems();
    expect(text(page.querySelector('[aria-live]'))).toBe('1\u201350 of 120');
    await click(labelled(page, 'Next page'));
    expect(last(ITEMS).query).toMatchObject({ offset: 50 });
    expect(text(page.querySelector('[aria-live]'))).toBe('51\u2013100 of 120');
    await click(labelled(page, 'Previous page'));
    expect(last(ITEMS).query).toMatchObject({ offset: 0 });
  });

  it('steps back to the last page when the page it is on has vanished', async () => {
    server.items = manyItems(60);
    await openItems();
    await click(labelled(page, 'Next page'));
    expect(rows()).toHaveLength(10);

    server.items = manyItems(30);
    await refetch();
    expect(requests(ITEMS).map((sent) => sent.query.offset)).toEqual([0, 50, 50, 0]);
    expect(rows()).toHaveLength(30);
    expect(text(page.querySelector('[aria-live]'))).toBe('1\u201330 of 30');
  });

  it('keeps the rows on screen while it reloads and forgets the selected items that are gone', async () => {
    await openItems();
    await click(labelled(page, 'Select IMG_0001.jpg'));
    await click(labelled(page, 'Select IMG_0002.jpg'));

    const held = hold(ITEMS);
    await refetch();
    expect(held).toHaveLength(1);
    expect(rows()).toHaveLength(3);
    expect(page.querySelector('[aria-label="Loading items"]')).toBeNull();

    server.items = [
      makeTrashItem({ id: 1, name: 'IMG_0001.jpg' }),
      makeTrashItem({ id: 3, name: 'Trip', is_dir: true }),
    ];
    await answerHeld(held[0] as Held);
    expect(rows()).toHaveLength(2);
    expect(button(page, 'Restore selected (1)').disabled).toBe(false);

    server.routes.delete(ITEMS);
    server.items = defaultItems();
    await refetch();
    expect(rows()).toHaveLength(3);
    expect(button(page, 'Restore selected (1)').disabled).toBe(false);
    expect(labelled<HTMLInputElement>(page, 'Select IMG_0002.jpg').checked).toBe(false);
  });

  it('ignores the answer of a request that was superseded', async () => {
    await openItems();
    const held = hold(ITEMS, { honourAbort: false });
    await click(button(page, 'Size'));
    await click(button(page, 'Name'));
    expect(held).toHaveLength(2);
    expect((held[0] as Held).sent.signal?.aborted).toBe(true);
    expect((held[1] as Held).sent.signal?.aborted).toBe(false);

    server.items = [makeTrashItem({ id: 5, name: 'new.jpg' })];
    await answerHeld(held[1] as Held);
    server.items = [makeTrashItem({ id: 6, name: 'stale.jpg' })];
    await answerHeld(held[0] as Held);
    expect(rows().map((row) => text(row.querySelector('td:nth-child(2)')))).toEqual(['new.jpg']);
  });

  it('does not report the abort of a superseded request as an error', async () => {
    await openItems();
    hold(ITEMS);
    await click(button(page, 'Size'));
    await click(button(page, 'Name'));
    expect(page.querySelector('[role="alert"]')).toBeNull();
  });
});

describe('apm-trash-page deletions', () => {
  it('asks before it restores a deletion, with the numbers, then queues the restore', async () => {
    await mountPage();
    await click(button(page, 'Restore all'));
    expect(await shownConfirm()).toEqual({
      title: 'Restore 3 items?',
      message:
        `3 items (6 KB) deleted on ${formatFileTime(TRASHED_AT)} go back to their original ` +
        'folders on volume photos. A name that is already taken gets a number, and an identical ' +
        'file stays in the trash.',
      buttons: ['Cancel', 'Restore 3 items'],
    });
    expect(requests(RESTORE)).toHaveLength(0);

    await pressConfirm('Restore 3 items');
    await flush();
    expect(requests(RESTORE)).toHaveLength(1);
    expect(last(RESTORE).body).toEqual({
      target: { kind: 'batch', volume_id: 1, batch: '20250101-120000-t7' },
      conflict_policy: null,
    });
    expect(messages()).toEqual([['info', 'Restoring 3 items from the trash']]);
  });

  it('does nothing when the question is cancelled', async () => {
    await mountPage();
    await click(button(page, 'Restore all'));
    await pressConfirm('Cancel');
    await flush();
    expect(requests(RESTORE)).toHaveLength(0);
    expect(messages()).toEqual([]);
    expect(button(page, 'Restore all').disabled).toBe(false);
  });

  it('says it for one item in the singular', async () => {
    server.batches = [makeTrashBatch({ items: 1, bytes: 2_048 })];
    await mountPage();
    await click(button(page, 'Restore all'));
    const shown = await shownConfirm();
    expect(shown.title).toBe('Restore 1 item?');
    expect(shown.buttons).toEqual(['Cancel', 'Restore 1 item']);
  });

  it.each([
    ['rename', 'A name that is already taken gets a number.'],
    ['skip', 'An item whose name is already taken stays in the trash.'],
    ['replace', 'A file that is already there goes to the trash and is replaced.'],
  ] as const)(
    'tells what the %s policy of the settings does',
    async (conflict_policy, sentence) => {
      server.summary = makeTrashSummary({ conflict_policy });
      await mountPage();
      await click(button(page, 'Restore all'));
      expect((await shownConfirm()).message).toContain(sentence);
      await pressConfirm('Restore 3 items');
      await flush();
      expect((last(RESTORE).body as { conflict_policy: unknown }).conflict_policy).toBeNull();
    },
  );

  it('keeps the actions waiting while a question is open', async () => {
    await mountPage();
    await click(button(page, 'Restore all'));
    await shownConfirm();
    expect(button(page, 'Restore all').disabled).toBe(true);
    expect(button(page, EMPTY_TRASH).disabled).toBe(true);
    expect(button(page, 'Look for untracked items').disabled).toBe(true);

    await pressConfirm('Cancel');
    await flush();
    expect(button(page, 'Restore all').disabled).toBe(false);
    expect(button(page, EMPTY_TRASH).disabled).toBe(false);
    expect(button(page, 'Look for untracked items').disabled).toBe(false);
  });

  it('disables "Restore all" on a volume that is offline or read-only, and says why', async () => {
    server.summary = makeTrashSummary({ volumes: [{ ...photos, online: false }] });
    await mountPage();
    const restore = button(page, 'Restore all');
    const reason = 'Volume photos is offline: its items cannot be restored until it is back.';
    expect(restore.disabled).toBe(true);
    expect(restore.title).toBe(reason);
    expect(restore.parentElement?.title).toBe(reason);
    await click(restore);
    expect(requests(RESTORE)).toHaveLength(0);

    server.summary = makeTrashSummary({ volumes: [{ ...photos, writable: false }] });
    await refetch();
    expect(button(page, 'Restore all').title).toBe(
      'Volume photos is read-only: its items cannot be restored.',
    );
  });

  it('shows the items of one deletion on the Items tab, with a chip to leave it', async () => {
    await mountPage();
    await click(button(page, 'View items'));
    expect(openTab()).toBe('items');
    expect(last(ITEMS).query).toEqual({
      volume_id: 1,
      batch: '20250101-120000-t7',
      q: null,
      sort: 'trashed_at',
      order: null,
      offset: 0,
      limit: PAGE_SIZE,
    });
    expect(text(required(page, '.badge-soft'))).toBe(
      `Deletion of ${formatFileTime(TRASHED_AT)} on photos`,
    );
    expect(labelled<HTMLSelectElement>(page, 'Volume').value).toBe('1');

    await click(labelled(page, 'Show every deletion'));
    expect(last(ITEMS).query).toMatchObject({ volume_id: 1, batch: null });
    expect(page.querySelector('.badge-soft')).toBeNull();
  });

  it('forgets the search when a deletion is viewed', async () => {
    await openItems();
    type('trip');
    await advance(SEARCH_DEBOUNCE_MS);
    await click(tab('deletions'));
    await click(button(page, 'View items'));
    expect(last(ITEMS).query).toMatchObject({ q: null, batch: '20250101-120000-t7' });
    expect(labelled<HTMLInputElement>(page, 'Search the trash').value).toBe('');
  });

  it('narrows the deletions to the volume of the deletion that was viewed', async () => {
    server.summary = twoVolumes();
    server.batches = [makeTrashBatch({ volume_id: 2, volume: 'archive', batch: 'b2' })];
    await mountPage();
    await click(button(page, 'View items'));
    await click(tab('deletions'));
    expect(last(BATCHES).query).toEqual({ volume_id: 2, offset: 0, limit: PAGE_SIZE });
    expect(labelled<HTMLSelectElement>(page, 'Volume').value).toBe('2');
  });

  it('narrows both tabs to the volume that is picked, and back to all of them', async () => {
    server.summary = twoVolumes();
    await mountPage();
    pickVolume('2');
    await flush();
    expect(last(BATCHES).query).toEqual({ volume_id: 2, offset: 0, limit: PAGE_SIZE });

    await click(tab('items'));
    expect(last(ITEMS).query).toMatchObject({ volume_id: 2 });

    pickVolume('');
    await flush();
    expect(last(ITEMS).query).toMatchObject({ volume_id: null });
    await click(tab('deletions'));
    expect(last(BATCHES).query).toMatchObject({ volume_id: null });
  });

  it('offers every volume that holds trash, and marks one that is offline', async () => {
    server.summary = makeTrashSummary({ volumes: [photos, { ...archive, online: false }] });
    await mountPage();
    const options = [...labelled(page, 'Volume').querySelectorAll('option')];
    expect(options.map((option) => [option.value, text(option)])).toEqual([
      ['', 'All volumes'],
      ['1', 'photos'],
      ['2', 'archive (offline)'],
    ]);
  });

  it('falls back to all volumes when the volume it filters on no longer holds trash', async () => {
    server.summary = twoVolumes();
    await mountPage();
    pickVolume('2');
    await flush();

    server.summary = makeTrashSummary({ volumes: [photos] });
    await refetch();
    expect(requests(BATCHES).map((sent) => sent.query.volume_id)).toEqual([null, 2, 2, null]);
    expect(labelled<HTMLSelectElement>(page, 'Volume').value).toBe('');
  });

  it('shows the pages of the deletions and steps through them', async () => {
    server.batches = manyBatches(120);
    await mountPage();
    expect(rows()).toHaveLength(PAGE_SIZE);
    expect(text(page.querySelector('[aria-live]'))).toBe('1\u201350 of 120');
    await click(labelled(page, 'Next page'));
    expect(last(BATCHES).query).toEqual({ volume_id: null, offset: PAGE_SIZE, limit: PAGE_SIZE });
    expect(text(page.querySelector('[aria-live]'))).toBe('51\u2013100 of 120');
    await click(labelled(page, 'Previous page'));
    expect(last(BATCHES).query).toMatchObject({ offset: 0 });
  });

  it('steps back to the last page when the page it is on has vanished', async () => {
    server.batches = manyBatches(60);
    await mountPage();
    await click(labelled(page, 'Next page'));
    expect(rows()).toHaveLength(10);

    server.batches = manyBatches(30);
    await refetch();
    expect(requests(BATCHES).map((sent) => sent.query.offset)).toEqual([0, 50, 50, 0]);
    expect(rows()).toHaveLength(30);
  });
});

describe('apm-trash-page empty trash', () => {
  it('asks with the numbers on screen, then empties up to the newest id it saw', async () => {
    server.summary = makeTrashSummary({ volumes: [photos, archive], max_id: 99 });
    await mountPage();
    await click(button(page, EMPTY_TRASH));
    expect(await shownConfirm()).toEqual({
      title: 'Empty the trash?',
      message: 'Permanently delete 15 items (15 KB) from all volumes? This cannot be undone.',
      buttons: ['Cancel', 'Empty trash'],
    });
    expect(requests(EMPTY)).toHaveLength(0);

    await pressConfirm('Empty trash');
    await flush();
    expect(requests(EMPTY)).toHaveLength(1);
    expect(last(EMPTY).body).toEqual({ volume_id: null, up_to_id: 99 });
    expect(messages()).toEqual([['info', 'Emptying the trash']]);
  });

  it('keeps what was asked when the trash changes while the question is read', async () => {
    await mountPage();
    await click(button(page, EMPTY_TRASH));
    const asked = await shownConfirm();

    server.summary = makeTrashSummary({
      max_id: 120,
      volumes: [{ ...photos, items: 14, bytes: 14_336 }],
    });
    events.emit(taskEvent({ id: 50, kind: 'fs_trash', state: 'done' }));
    await advance(REFRESH_DEBOUNCE_MS);
    expect(text(page.querySelector('.stat'))).toBe('Items 14');
    expect(await shownConfirm()).toEqual(asked);

    await pressConfirm('Empty trash');
    await flush();
    expect(last(EMPTY).body).toEqual({ volume_id: null, up_to_id: 99 });
  });

  it('empties only the volume that is picked', async () => {
    server.summary = twoVolumes();
    await mountPage();
    pickVolume('2');
    await flush();

    await click(button(page, `Empty trash on archive${ELLIPSIS}`));
    expect(await shownConfirm()).toEqual({
      title: 'Empty the trash on archive?',
      message: 'Permanently delete 5 items (5 KB) from archive? This cannot be undone.',
      buttons: ['Cancel', 'Empty trash on archive'],
    });
    await pressConfirm('Empty trash on archive');
    await flush();
    expect(last(EMPTY).body).toEqual({ volume_id: 2, up_to_id: 99 });
  });

  it('empties the volume of the deletion that is being viewed', async () => {
    server.summary = twoVolumes();
    server.batches = [makeTrashBatch({ volume_id: 2, volume: 'archive', batch: 'b2' })];
    await mountPage();
    await click(button(page, 'View items'));
    await click(button(page, `Empty trash on archive${ELLIPSIS}`));
    await pressConfirm('Empty trash on archive');
    await flush();
    expect(last(EMPTY).body).toEqual({ volume_id: 2, up_to_id: 99 });
  });

  it('says what stays behind on a volume that is offline or read-only', async () => {
    server.summary = makeTrashSummary({ volumes: [photos, { ...archive, online: false }] });
    await mountPage();
    await click(button(page, EMPTY_TRASH));
    expect((await shownConfirm()).message).toBe(
      'Permanently delete 15 items (15 KB) from all volumes? This cannot be undone. ' +
        'Items on offline or read-only volumes (archive) cannot be deleted: they stay in the trash.',
    );
    await pressConfirm('Empty trash');
    await flush();
    expect(last(EMPTY).body).toEqual({ volume_id: null, up_to_id: 99 });
  });

  it('does nothing when the question is cancelled', async () => {
    await mountPage();
    await click(button(page, EMPTY_TRASH));
    await pressConfirm('Cancel');
    await flush();
    expect(requests(EMPTY)).toHaveLength(0);
    expect(messages()).toEqual([]);
    expect(button(page, EMPTY_TRASH).disabled).toBe(false);
  });

  it.each([
    ['the trash is empty', () => makeTrashSummary({ volumes: [] }), 'The trash is empty.'],
    [
      'its volume is offline',
      () => makeTrashSummary({ volumes: [{ ...photos, online: false }] }),
      'Volume photos is offline: its items cannot be deleted until it is back.',
    ],
    [
      'its volume is read-only',
      () => makeTrashSummary({ volumes: [{ ...photos, writable: false }] }),
      'Volume photos is read-only: its items cannot be deleted.',
    ],
  ])('is disabled, with the reason as a tooltip, when %s', async (_name, summary, reason) => {
    server.summary = summary();
    await mountPage();
    const empty = button(page, EMPTY_TRASH);
    expect(empty.disabled).toBe(true);
    expect(empty.title).toBe(reason);
    expect(empty.parentElement?.title).toBe(reason);
    await click(empty);
    expect(requests(EMPTY)).toHaveLength(0);
  });

  it('is disabled for a volume that holds no trash', async () => {
    server.summary = twoVolumes();
    await mountPage();
    pickVolume('2');
    await flush();
    server.summary = makeTrashSummary({ volumes: [photos, { ...archive, items: 0, bytes: 0 }] });
    await refetch();
    expect(button(page, `Empty trash on archive${ELLIPSIS}`).title).toBe(
      'The trash on archive is empty.',
    );
  });

  it('says so when the server found nothing left to delete, and reloads', async () => {
    refuse(
      EMPTY,
      new ApiError({
        status: 409,
        code: 'trash_already_empty',
        message: 'no item of the trash matches: it was already emptied',
      }),
    );
    await mountPage();
    await confirmEmpty();
    expect(messages()).toEqual([['info', 'The trash was already empty']]);
    expect(requests(BATCHES)).toHaveLength(2);
    expect(button(page, EMPTY_TRASH).disabled).toBe(false);
  });

  it('reports any other failure and lets the user try again', async () => {
    refuse(
      EMPTY,
      new ApiError({ status: 409, code: 'volume_offline', message: 'Volume archive is offline' }),
    );
    await mountPage();
    await confirmEmpty();
    expect(messages()).toEqual([['error', 'Volume archive is offline']]);
    expect(button(page, EMPTY_TRASH).disabled).toBe(false);
  });

  it('uses its own words for a failure that says nothing', async () => {
    refuse(EMPTY, undefined);
    await mountPage();
    await confirmEmpty();
    expect(messages()).toEqual([['error', 'Could not empty the trash']]);
  });
});

describe('apm-trash-page more actions', () => {
  it('has a menu with "Look for untracked items", which checks the trash', async () => {
    await mountPage();
    const trigger = labelled(page, 'More actions');
    expect(trigger.getAttribute('role')).toBe('button');
    expect(trigger.nextElementSibling?.contains(button(page, 'Look for untracked items'))).toBe(
      true,
    );

    await click(button(page, 'Look for untracked items'));
    expect(requests(SYNC)).toHaveLength(1);
    expect(last(SYNC).body).toBeUndefined();
    expect(messages()).toEqual([['info', 'Checking the trash']]);

    events.emit(
      taskEvent({ id: 9, kind: 'fs_trash', state: 'done', title: 'The trash matches the disk' }),
    );
    await flush();
    expect(messages()).toEqual([
      ['info', 'Checking the trash'],
      ['success', 'The trash matches the disk'],
    ]);
  });

  it('warns when the check kept something back', async () => {
    await mountPage();
    await click(button(page, 'Look for untracked items'));
    events.emit(
      taskEvent({
        id: 9,
        kind: 'fs_trash',
        state: 'done',
        title: 'Checked the trash: kept 3 items whose file is missing (is a share unmounted?)',
      }),
    );
    await flush();
    expect(messages().at(-1)).toEqual([
      'warning',
      'Checked the trash: kept 3 items whose file is missing (is a share unmounted?)',
    ]);
  });

  it('reports a check that could not be queued', async () => {
    refuse('POST /api/trash/sync', serverError());
    await mountPage();
    await click(button(page, 'Look for untracked items'));
    expect(messages()).toEqual([['error', 'The server is down']]);
  });

  it('closes the menu on Escape', async () => {
    await mountPage();
    const trigger = labelled(page, 'More actions');
    trigger.focus();
    expect(document.activeElement).toBe(trigger);
    press(trigger, 'Escape');
    expect(document.activeElement).not.toBe(trigger);
  });
});

describe('apm-trash-page tasks', () => {
  it('toasts the start of a restore, then how it ended', async () => {
    await mountPage();
    await confirmRestoreAll();
    expect(messages()).toEqual([['info', 'Restoring 3 items from the trash']]);

    events.emit(taskEvent({ id: 7, kind: 'fs_move', state: 'running', items_done: 1 }));
    await flush();
    expect(messages()).toHaveLength(1);

    events.emit(taskEvent({ id: 7, kind: 'fs_move', state: 'done', title: 'Restored 3 items' }));
    await flush();
    expect(messages()).toEqual([
      ['info', 'Restoring 3 items from the trash'],
      ['success', 'Restored 3 items'],
    ]);
  });

  it.each([
    [{ state: 'done', title: 'Restored 3 items' }, 'success', 'Restored 3 items'],
    [
      { state: 'done', title: 'Restored 2 of 3 items, 1 left in the trash' },
      'warning',
      'Restored 2 of 3 items, 1 left in the trash',
    ],
    [
      { state: 'failed', error: 'Restored 1 of 3 items (2 failed): a.jpg (io: denied)' },
      'error',
      'Restored 1 of 3 items (2 failed): a.jpg (io: denied)',
    ],
    [
      { state: 'interrupted', error: null },
      'error',
      'Restoring 3 items from the trash was interrupted',
    ],
    [{ state: 'cancelled' }, 'info', 'Restoring 3 items from the trash was cancelled'],
  ] as const satisfies readonly (readonly [Partial<TaskDto>, string, string])[])(
    'tells how a restore ended: %j',
    async (ending, kind, message) => {
      await mountPage();
      await confirmRestoreAll();
      events.emit(
        taskEvent({ id: 7, kind: 'fs_move', title: 'Restoring 3 items from the trash', ...ending }),
      );
      await flush();
      expect(messages().at(-1)).toEqual([kind, message]);
    },
  );

  it('toasts how a task ended at once when it ended before the request was answered', async () => {
    await mountPage();
    const held = hold(RESTORE);
    await click(button(page, 'Restore all'));
    await pressConfirm('Restore 3 items');
    await flush();
    expect(held).toHaveLength(1);

    events.emit(taskEvent({ id: 7, kind: 'fs_move', state: 'done', title: 'Restored 3 items' }));
    await flush();
    expect(messages()).toEqual([]);

    (held[0] as Held).answer(
      makeTask({
        id: 7,
        kind: 'fs_move',
        state: 'queued',
        title: 'Restoring 3 items from the trash',
      }),
    );
    await flush();
    expect(messages()).toEqual([['success', 'Restored 3 items']]);
  });

  it('toasts the outcome and not the start when the answer is a task that is over', async () => {
    server.routes.set(RESTORE, () =>
      makeTask({ id: 7, kind: 'fs_move', state: 'done', title: 'Restored 3 items' }),
    );
    await mountPage();
    await confirmRestoreAll();
    expect(messages()).toEqual([['success', 'Restored 3 items']]);
  });

  it('does not toast the ending of a task it did not start, but looks at the trash again', async () => {
    await mountPage();
    events.emit(taskEvent({ id: 31, kind: 'fs_move', state: 'done', title: 'Moved 4 files' }));
    await advance(REFRESH_DEBOUNCE_MS);
    expect(messages()).toEqual([]);
    expect(requests(BATCHES)).toHaveLength(2);
  });

  it('toasts an ending once', async () => {
    await mountPage();
    await confirmRestoreAll();
    const success = vi.spyOn(toast, 'success');
    const ended = taskEvent({ id: 7, kind: 'fs_move', state: 'done', title: 'Restored 3 items' });
    events.emit(ended);
    events.emit(ended);
    await flush();
    expect(success.mock.calls).toEqual([['Restored 3 items']]);
  });

  it('reports a restore that is refused and enables the buttons again', async () => {
    refuse(
      RESTORE,
      new ApiError({ status: 409, code: 'volume_offline', message: 'Volume photos is offline' }),
    );
    await mountPage();
    await confirmRestoreAll();
    expect(messages()).toEqual([['error', 'Volume photos is offline']]);
    expect(button(page, 'Restore all').disabled).toBe(false);
  });

  it('uses its own words for a restore that fails without a message', async () => {
    refuse(RESTORE, undefined);
    await openItems();
    await click(labelled(page, 'Select IMG_0001.jpg'));
    await click(button(page, 'Restore selected (1)'));
    expect(messages()).toEqual([['error', 'Could not restore the items']]);
    expect(button(page, 'Restore selected (1)').disabled).toBe(false);
  });
});

describe('apm-trash-page refreshing', () => {
  it.each(['fs_move', 'fs_trash', 'plan_execute', 'trash_empty'] as const)(
    'looks at the trash again when a %s task ends, after a short pause',
    async (kind) => {
      await mountPage();
      events.emit(taskEvent({ id: 40, kind, state: 'done' }));
      await advance(REFRESH_DEBOUNCE_MS - 1);
      expect(requests(BATCHES)).toHaveLength(1);
      await advance(1);
      expect(requests(BATCHES)).toHaveLength(2);
    },
  );

  it.each(['done', 'failed', 'cancelled', 'interrupted'] as const)(
    'looks at the trash again when a task is %s',
    async (state) => {
      await mountPage();
      events.emit(taskEvent({ id: 40, kind: 'fs_move', state }));
      await advance(REFRESH_DEBOUNCE_MS);
      expect(requests(BATCHES)).toHaveLength(2);
    },
  );

  it('turns a burst of endings into one request', async () => {
    await mountPage();
    for (let id = 40; id < 45; id++) {
      events.emit(taskEvent({ id, kind: 'fs_move', state: 'done' }));
      await advance(100);
    }
    await advance(REFRESH_DEBOUNCE_MS);
    expect(requests(BATCHES)).toHaveLength(2);
  });

  it('ignores a task that is still going and a task that cannot touch the trash', async () => {
    await mountPage();
    events.emit(taskEvent({ id: 40, kind: 'fs_move', state: 'running' }));
    events.emit(taskEvent({ id: 41, kind: 'trash_empty', state: 'queued' }));
    events.emit(taskEvent({ id: 42, kind: 'index', state: 'done' }));
    events.emit(taskEvent({ id: 43, kind: 'immich_sync', state: 'done' }));
    await advance(REFRESH_DEBOUNCE_MS * 4);
    expect(requests(BATCHES)).toHaveLength(1);
  });

  it('looks at the trash again when the window gets the focus back', async () => {
    await mountPage();
    window.dispatchEvent(new Event('focus'));
    window.dispatchEvent(new Event('focus'));
    await advance(REFRESH_DEBOUNCE_MS);
    expect(requests(BATCHES)).toHaveLength(2);
  });

  it('looks at the trash again after the event stream reconnects', async () => {
    vi.stubGlobal('EventSource', FakeEventSource);
    FakeEventSource.reset();
    events.start();
    FakeEventSource.last().open();
    await mountPage();

    FakeEventSource.last().error();
    await advance(SERVER_RETRY_MS * 2);
    FakeEventSource.last().open();
    await advance(REFRESH_DEBOUNCE_MS);
    expect(requests(BATCHES)).toHaveLength(2);
  });

  it('reloads the open tab and the other one when it is opened', async () => {
    await openItems();
    await refetch();
    expect(requests(ITEMS)).toHaveLength(2);
    expect(requests(BATCHES)).toHaveLength(1);

    await click(tab('deletions'));
    expect(requests(BATCHES)).toHaveLength(2);
  });

  it('keeps the deletions on screen while they reload', async () => {
    await mountPage();
    const held = hold(BATCHES);
    await refetch();
    expect(held).toHaveLength(1);
    expect(rows()).toHaveLength(1);
    expect(page.querySelector('[aria-label="Loading deletions"]')).toBeNull();

    server.batches = [makeTrashBatch(), makeTrashBatch({ batch: 'b2' })];
    await answerHeld(held[0] as Held);
    expect(rows()).toHaveLength(2);
  });

  it('takes the totals from every answer', async () => {
    await mountPage();
    server.summary = makeTrashSummary({ volumes: [{ ...photos, items: 25, bytes: 20_480 }] });
    await refetch();
    expect([...page.querySelectorAll('.stat-value')].map(text)).toEqual(['25', '20 KB']);
  });
});

describe('apm-trash-page errors and empty states', () => {
  it('shows an alert with Retry when the deletions cannot be loaded', async () => {
    refuse(BATCHES, serverError());
    await mountPage();
    const alert = required(page, '[role="alert"]');
    expect(text(alert)).toBe('Could not load the deletions The server is down Retry');
    expect(page.querySelector('[aria-label="Loading deletions"]')).toBeNull();

    server.routes.delete(BATCHES);
    await click(button(alert, 'Retry'));
    expect(page.querySelector('[role="alert"]')).toBeNull();
    expect(rows()).toHaveLength(1);
  });

  it('uses its own words for a failure that says nothing', async () => {
    refuse(BATCHES, undefined);
    await mountPage();
    expect(text(required(page, '[role="alert"]'))).toBe(
      'Could not load the deletions Could not reach the server Retry',
    );
  });

  it('shows the alert instead of the table when the first load of the items fails', async () => {
    refuse(ITEMS, serverError());
    await openItems();
    expect(text(required(page, '[role="alert"]'))).toBe(
      'Could not load the items The server is down Retry',
    );
    expect(page.querySelector('table')).toBeNull();

    server.routes.delete(ITEMS);
    await click(button(page, 'Retry'));
    expect(page.querySelector('[role="alert"]')).toBeNull();
    expect(rows()).toHaveLength(3);
  });

  it('keeps the table below the alert when a reload of the items fails', async () => {
    await openItems();
    refuse(ITEMS, serverError());
    await refetch();
    expect(text(required(page, '[role="alert"]'))).toContain('Could not load the items');
    expect(rows()).toHaveLength(3);
  });

  it('says the trash is empty, with the retention rule', async () => {
    server.batches = [];
    server.items = [];
    server.summary = makeTrashSummary({ volumes: [], retention_days: 30 });
    await mountPage();
    expect(text(required(page, '[role="status"]'))).toBe(
      'The trash is empty Items are deleted automatically 30 days after they were trashed.',
    );
    await click(tab('items'));
    expect(text(required(page, '[role="status"]'))).toBe(
      'The trash is empty Items are deleted automatically 30 days after they were trashed.',
    );
  });

  it('says nothing matches when a volume has no deletion, and clears the filters', async () => {
    server.summary = twoVolumes();
    await mountPage();
    server.batches = [];
    pickVolume('2');
    await flush();
    expect(text(required(page, '[role="status"]'))).toContain('Nothing matches');
    expect(text(required(page, '[role="status"]'))).toContain(
      'No item in the trash matches these filters.',
    );

    server.batches = [makeTrashBatch()];
    await click(button(page, 'Clear filters'));
    expect(last(BATCHES).query).toMatchObject({ volume_id: null });
    expect(rows()).toHaveLength(1);
  });

  it('says nothing matches when a search finds no item, and clears the search', async () => {
    await openItems();
    server.items = [];
    type('zzz');
    await advance(SEARCH_DEBOUNCE_MS);
    expect(text(required(page, '[role="status"]'))).toContain('Nothing matches');

    server.items = defaultItems();
    await click(button(page, 'Clear filters'));
    expect(last(ITEMS).query).toMatchObject({ q: null, volume_id: null });
    expect(labelled<HTMLInputElement>(page, 'Search the trash').value).toBe('');
    expect(rows()).toHaveLength(3);
  });
});

describe('apm-trash-page when it is removed', () => {
  it('cancels the request that is on its way', async () => {
    const held = hold(BATCHES);
    await mountPage();
    expect((held[0] as Held).sent.signal?.aborted).toBe(false);
    page.remove();
    expect((held[0] as Held).sent.signal?.aborted).toBe(true);
  });

  it('forgets its timers: no search, no reload', async () => {
    await openItems();
    type('trip');
    events.emit(taskEvent({ id: 3, kind: 'fs_move', state: 'done' }));
    window.dispatchEvent(new Event('focus'));
    const before = server.sent.length;
    page.remove();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(server.sent).toHaveLength(before);
  });

  it('stops listening for tasks, focus and reconnections', async () => {
    vi.stubGlobal('EventSource', FakeEventSource);
    FakeEventSource.reset();
    events.start();
    FakeEventSource.last().open();
    await mountPage();
    await confirmRestoreAll();
    const before = server.sent.length;
    const outcomes = [
      vi.spyOn(toast, 'success'),
      vi.spyOn(toast, 'warning'),
      vi.spyOn(toast, 'error'),
    ];

    page.remove();
    events.emit(taskEvent({ id: 7, kind: 'fs_move', state: 'done', title: 'Restored 3 items' }));
    window.dispatchEvent(new Event('focus'));
    FakeEventSource.last().error();
    await vi.advanceTimersByTimeAsync(SERVER_RETRY_MS * 2);
    FakeEventSource.last().open();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(server.sent).toHaveLength(before);
    for (const outcome of outcomes) expect(outcome).not.toHaveBeenCalled();
  });
});
