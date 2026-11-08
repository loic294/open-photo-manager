import { afterEach, describe, expect, it, vi } from 'vitest';
import { api, type RequestOptions } from '@/api/client';
import { ApiError } from '@/api/errors';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import type { Facets } from '@/api/generated/Facets';
import type { Filter } from '@/api/generated/Filter';
import type { ServerEvent } from '@/api/generated/ServerEvent';
import type { TaskKind } from '@/api/generated/TaskKind';
import type { TaskState } from '@/api/generated/TaskState';
import { events, SERVER_RETRY_MS } from '@/api/sse';
import '@/components/apm-filter-bar';
import type { ApmFilterBar } from '@/components/apm-filter-bar';
import type { ApmMultiSelect } from '@/components/apm-multi-select';
import type { ApmSegmented } from '@/components/apm-segmented';
import { FakeEventSource } from '@/test/fake-event-source';
import { mount } from '@/test/helpers';
import { EMPTY_FILTER, type ScopeLabels } from '@/util/filter';
import { formatCount } from '@/util/format-count';

interface Logged {
  type: string;
  detail: unknown;
}

interface FacetCall {
  path: string;
  body: { volume_id: number | null; dir: string; filter: Filter; plan: unknown };
  signal: AbortSignal | undefined;
  answer(facets: Facets): void;
  fail(error: unknown): void;
}

function where(volume_id: number | null, dir = ''): BrowseRequest {
  return { volume_id, dir, filter: EMPTY_FILTER, plan: null };
}

const TRIPS = where(1, 'trips');
const BACKUP = where(2);

const FACETS: Facets = {
  cameras: [
    { value: 'Sony A7 IV', count: 1200 },
    { value: 'Canon EOS R5', count: 340 },
  ],
  tags: [
    { value: 'sunset', count: 30 },
    { value: 'family', count: 12 },
  ],
  ratings: [
    { value: '5', count: 7 },
    { value: '3', count: 2 },
  ],
};

const OTHER_FACETS: Facets = {
  cameras: [{ value: 'Nikon Z6', count: 4 }],
  tags: [{ value: 'beach', count: 2 }],
  ratings: [{ value: '1', count: 4 }],
};

const FIRST_PANE: Facets = {
  cameras: [
    { value: 'Sony A7 IV', count: 10 },
    { value: 'Canon EOS R5', count: 5 },
  ],
  tags: [{ value: 'sunset', count: 3 }],
  ratings: [{ value: '5', count: 2 }],
};

const SECOND_PANE: Facets = {
  cameras: [
    { value: 'canon eos r5', count: 2 },
    { value: 'Fujifilm X-T4', count: 1 },
  ],
  tags: [{ value: 'family', count: 4 }],
  ratings: [{ value: '3', count: 1 }],
};

const cleanups: Array<() => void> = [];

afterEach(() => {
  events.stop();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  FakeEventSource.reset();
  for (const undo of cleanups.splice(0)) undo();
});

function filterWith(patch: Partial<Filter> = {}): Filter {
  return { ...EMPTY_FILTER, ...patch };
}

function nth<T>(list: readonly T[], index: number): T {
  const item = list[index];
  if (item === undefined) throw new Error(`no entry #${index}, there are ${list.length}`);
  return item;
}

function required<T extends Element>(root: ParentNode, selector: string): T {
  const found = root.querySelector<T>(selector);
  if (!found) throw new Error(`nothing matches "${selector}"`);
  return found;
}

function listen(target: EventTarget, type: string, handler: (event: Event) => void): void {
  target.addEventListener(type, handler);
  cleanups.push(() => target.removeEventListener(type, handler));
}

function record(host: ApmFilterBar): Logged[] {
  const log: Logged[] = [];
  for (const type of [
    'filter-change',
    'hide-unmatched-change',
    'scope-change',
    'selection-clear',
  ] as const) {
    host.addEventListener(type, (event) => log.push({ type, detail: event.detail }));
  }
  return log;
}

function send(target: Element, type: string, detail: unknown): void {
  target.dispatchEvent(new CustomEvent(type, { detail, bubbles: true }));
}

function fsChange(volume_id: number, ...dirs: string[]): ServerEvent {
  return { type: 'fs_change', volume_id, dirs };
}

function taskEvent(state: TaskState, kind: TaskKind = 'index'): ServerEvent {
  return {
    type: 'task',
    task: {
      id: 1,
      kind,
      title: 'Index photos',
      state,
      items_done: 0,
      items_total: 100,
      bytes_done: 0,
      bytes_total: null,
      speed: null,
      eta_secs: null,
      error: null,
      started_at: 1,
      finished_at: null,
    },
  };
}

/**
 * Replaces `POST /api/facets`: every call waits until the test answers or fails it. By default a
 * call rejects with an `AbortError` when its signal aborts, like `fetch`; `honourAbort: false`
 * models a server that answers anyway, to prove the bar drops what it no longer wants.
 */
function fakeFacetApi(options: { honourAbort?: boolean } = {}): FacetCall[] {
  const { honourAbort = true } = options;
  const calls: FacetCall[] = [];
  vi.spyOn(api, 'post').mockImplementation(
    ((path: string, request: RequestOptions = {}) =>
      new Promise<unknown>((resolve, reject) => {
        if (honourAbort) {
          request.signal?.addEventListener('abort', () =>
            reject(new DOMException('Aborted', 'AbortError')),
          );
        }
        calls.push({
          path,
          body: request.body as FacetCall['body'],
          signal: request.signal,
          answer: resolve,
          fail: reject,
        });
      })) as typeof api.post,
  );
  return calls;
}

// Lets every promise callback and Lit update that is already due run (hosts and their children).
async function flush(): Promise<void> {
  if (vi.isFakeTimers()) await vi.advanceTimersByTimeAsync(0);
  else await new Promise<void>((resolve) => setTimeout(resolve, 0));
}

async function advance(ms: number): Promise<void> {
  await vi.advanceTimersByTimeAsync(ms);
}

async function answer(call: FacetCall, facets: Facets): Promise<void> {
  call.answer(facets);
  await flush();
}

const SERVER_ERROR = new ApiError({ status: 500, code: 'internal', message: 'The server is down' });

async function fail(call: FacetCall, error: unknown = SERVER_ERROR): Promise<void> {
  call.fail(error);
  await flush();
}

async function rendered(host: ApmFilterBar): Promise<void> {
  await host.updateComplete;
  const children = host.querySelectorAll<ApmMultiSelect | ApmSegmented>(
    'apm-multi-select, apm-segmented',
  );
  await Promise.all(Array.from(children, (child) => child.updateComplete));
}

async function mountBar(props: Partial<ApmFilterBar> = {}): Promise<ApmFilterBar> {
  const host = await mount('apm-filter-bar', props);
  await rendered(host);
  return host;
}

async function feed(host: ApmFilterBar, props: Partial<ApmFilterBar>): Promise<void> {
  Object.assign(host, props);
  await rendered(host);
}

/** A bar that gets its locations after the fake timers started, so its debounce is fake. */
async function mountAt(
  contexts: BrowseRequest[],
  props: Partial<ApmFilterBar> = {},
): Promise<ApmFilterBar> {
  const host = await mountBar(props);
  vi.useFakeTimers();
  await feed(host, { contexts });
  return host;
}

/** A bar at `TRIPS` whose first request (the first of `calls`) was answered. */
async function mountLoaded(
  calls: FacetCall[],
  facets: Facets = FACETS,
  props: Partial<ApmFilterBar> = {},
): Promise<ApmFilterBar> {
  const host = await mountAt([TRIPS], props);
  await advance(250);
  await answer(nth(calls, 0), facets);
  return host;
}

function camerasOf(host: ApmFilterBar): ApmMultiSelect {
  return required<ApmMultiSelect>(host, 'apm-multi-select[label="Cameras"]');
}

function tagsOf(host: ApmFilterBar): ApmMultiSelect {
  return required<ApmMultiSelect>(host, 'apm-multi-select[label="Tags"]');
}

function ratingOf(host: ApmFilterBar): ApmSegmented {
  return required<ApmSegmented>(host, 'apm-segmented[label="Rating"]');
}

function flagsOf(host: ApmFilterBar): ApmSegmented {
  return required<ApmSegmented>(host, 'apm-segmented[label="Flags"]');
}

function scopeOf(host: ApmFilterBar): ApmSegmented | null {
  return host.querySelector<ApmSegmented>('apm-segmented[label="Apply to"]');
}

function nameOf(button: HTMLElement): string {
  return button.getAttribute('aria-label') ?? button.querySelector('span')?.textContent ?? '';
}

function segmentNames(group: ApmSegmented): string[] {
  return Array.from(group.querySelectorAll('button'), nameOf);
}

/** The button called `name`, whether or not it carries a count after the name. */
function segment(group: ApmSegmented, name: string): HTMLButtonElement {
  const found = Array.from(group.querySelectorAll('button')).find((candidate) => {
    const text = nameOf(candidate);
    return text === name || text.startsWith(`${name} (`);
  });
  if (!found) throw new Error(`no segment "${name}"`);
  return found;
}

function clearButton(host: ApmFilterBar): HTMLButtonElement {
  const found = Array.from(host.querySelectorAll('button')).find((button) =>
    button.textContent?.includes('Clear filters'),
  );
  if (!found) throw new Error('no "Clear filters" button');
  return found;
}

function toggle(host: ApmFilterBar): HTMLInputElement {
  return required<HTMLInputElement>(host, 'input[role="switch"]');
}

function selectionChip(host: ApmFilterBar): HTMLElement | null {
  return host.querySelector<HTMLElement>('.badge-info');
}

function selectionRemove(host: ApmFilterBar): HTMLButtonElement {
  return required<HTMLButtonElement>(host, 'button[aria-label="Remove selection filter"]');
}

function failureHint(host: ApmFilterBar): HTMLElement | null {
  return host.querySelector<HTMLElement>('p[role="status"]');
}

function retryButton(host: ApmFilterBar): HTMLButtonElement {
  return required<HTMLButtonElement>(required(host, 'p[role="status"]'), 'button');
}

function precedes(first: Node, second: Node): boolean {
  return (first.compareDocumentPosition(second) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0;
}

async function openPanel(select: ApmMultiSelect): Promise<void> {
  required<HTMLElement>(select, 'details > summary').click();
  await select.updateComplete;
}

function checkboxes(select: ApmMultiSelect): HTMLInputElement[] {
  return Array.from(select.querySelectorAll<HTMLInputElement>('input[type="checkbox"]'));
}

function rowNames(select: ApmMultiSelect): string[] {
  return checkboxes(select).map(
    (box) => box.closest('label')?.querySelector('span')?.textContent ?? '',
  );
}

async function pick(select: ApmMultiSelect, value: string): Promise<void> {
  await openPanel(select);
  const box = checkboxes(select)[rowNames(select).indexOf(value)];
  if (!box) throw new Error(`no row "${value}"`);
  box.click();
  await select.updateComplete;
}

async function typeIn(select: ApmMultiSelect, text: string): Promise<void> {
  const input = required<HTMLInputElement>(select, 'input[type="search"]');
  input.value = text;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await select.updateComplete;
}

function chipsOf(select: ApmMultiSelect): string[] {
  return Array.from(
    select.querySelectorAll('ul[role="list"] > li > span'),
    (span) => span.textContent ?? '',
  );
}

function chipRemove(select: ApmMultiSelect, value: string): HTMLButtonElement {
  return required<HTMLButtonElement>(
    select,
    `ul[role="list"] button[aria-label="Remove ${value}"]`,
  );
}

function connectEvents(): void {
  vi.stubGlobal('EventSource', FakeEventSource);
  FakeEventSource.reset();
  events.start();
  FakeEventSource.last().open();
}

async function reconnectEvents(): Promise<void> {
  FakeEventSource.last().error();
  await advance(SERVER_RETRY_MS * 2);
  FakeEventSource.last().open();
  await flush();
}

describe('apm-filter-bar structure', () => {
  it('is a block host holding one labelled group of controls', async () => {
    const host = await mountBar();

    expect(host.classList.contains('block')).toBe(true);
    const group = required(host, 'div[role="group"]');
    expect(group.getAttribute('aria-label')).toBe('Filters');
    expect(group.classList.contains('flex')).toBe(true);
    expect(group.classList.contains('flex-wrap')).toBe(true);
    // The dropdown panels hang out of the bar: nothing may clip them.
    expect(group.classList.contains('overflow-hidden')).toBe(false);
  });

  it('lays the controls out in a fixed order', async () => {
    const host = await mountBar({ scope: 'both', filter: filterWith({ selection_id: 1 }) });

    const labels = Array.from(host.querySelectorAll('apm-segmented, apm-multi-select'), (child) =>
      child.getAttribute('label'),
    );
    expect(labels).toEqual(['Apply to', 'Cameras', 'Rating', 'Flags', 'Tags']);
    const chip = required(host, '.badge-info');
    expect(precedes(tagsOf(host), chip)).toBe(true);
    expect(precedes(chip, clearButton(host))).toBe(true);
    expect(precedes(clearButton(host), toggle(host))).toBe(true);
  });

  it('offers unrated and one to five stars', async () => {
    const host = await mountBar();

    expect(ratingOf(host).mode).toBe('multi');
    expect(ratingOf(host).options.map((option) => option.value)).toEqual([
      '0',
      '1',
      '2',
      '3',
      '4',
      '5',
    ]);
    expect(segmentNames(ratingOf(host))).toEqual([
      'Unrated',
      '1 star',
      '2 stars',
      '3 stars',
      '4 stars',
      '5 stars',
    ]);
  });

  it('offers the two flags', async () => {
    const host = await mountBar();

    expect(flagsOf(host).mode).toBe('multi');
    expect(flagsOf(host).options.map((option) => option.value)).toEqual(['selected', 'rejected']);
    expect(segmentNames(flagsOf(host))).toEqual(['Selected', 'Rejected']);
  });

  it('lets tags be typed and cameras not', async () => {
    const host = await mountBar();

    expect(tagsOf(host).allowCustom).toBe(true);
    expect(camerasOf(host).allowCustom).toBe(false);
  });

  it('shows what the filter holds', async () => {
    const host = await mountBar({
      filter: filterWith({
        cameras: ['Sony A7 IV'],
        ratings: [3, 5],
        flags: ['rejected'],
        tags: ['sunset'],
      }),
    });

    expect(camerasOf(host).selected).toEqual(['Sony A7 IV']);
    expect(tagsOf(host).selected).toEqual(['sunset']);
    expect(ratingOf(host).value).toEqual(['3', '5']);
    expect(flagsOf(host).value).toEqual(['rejected']);
    expect(segment(ratingOf(host), '3 stars').getAttribute('aria-pressed')).toBe('true');
    expect(segment(ratingOf(host), '4 stars').getAttribute('aria-pressed')).toBe('false');
    expect(segment(flagsOf(host), 'Rejected').getAttribute('aria-pressed')).toBe('true');
    expect(segment(flagsOf(host), 'Selected').getAttribute('aria-pressed')).toBe('false');
  });

  it('starts quiet: nothing to clear, no chip, no hint and no request', async () => {
    const calls = fakeFacetApi();
    const host = await mountBar();

    expect(clearButton(host).disabled).toBe(true);
    expect(selectionChip(host)).toBeNull();
    expect(host.querySelector('p')).toBeNull();
    expect(calls).toHaveLength(0);
  });
});

describe('apm-filter-bar scope selector', () => {
  it('is hidden for a tool with a single pane', async () => {
    const host = await mountBar();

    expect(scopeOf(host)).toBeNull();
    expect(host.textContent).not.toContain('Apply to');
  });

  it('offers both, source and comparison and marks the current one', async () => {
    const host = await mountBar({ scope: 'source' });

    const selector = scopeOf(host);
    expect(selector).not.toBeNull();
    expect(selector?.mode).toBe('single');
    expect(host.textContent).toContain('Apply to');
    expect(selector?.options.map((option) => option.value)).toEqual([
      'both',
      'source',
      'comparison',
    ]);
    expect(segmentNames(required<ApmSegmented>(host, 'apm-segmented[label="Apply to"]'))).toEqual([
      'Both',
      'Source',
      'Comparison',
    ]);
    expect(selector?.value).toBe('source');
    const source = segment(
      required<ApmSegmented>(host, 'apm-segmented[label="Apply to"]'),
      'Source',
    );
    expect(source.getAttribute('aria-checked')).toBe('true');
  });

  it('renames the options with scopeLabels and keeps the names it was not given', async () => {
    const host = await mountBar({
      scope: 'both',
      scopeLabels: { both: 'Both panes', source: 'Left', comparison: 'Right' },
    });
    const selector = required<ApmSegmented>(host, 'apm-segmented[label="Apply to"]');
    expect(segmentNames(selector)).toEqual(['Both panes', 'Left', 'Right']);

    await feed(host, { scopeLabels: { source: 'Left' } as unknown as ScopeLabels });

    expect(segmentNames(selector)).toEqual(['Both', 'Left', 'Comparison']);
  });

  it('emits scope-change with the new scope and nothing else', async () => {
    const host = await mountBar({ scope: 'both' });
    const log = record(host);

    segment(required<ApmSegmented>(host, 'apm-segmented[label="Apply to"]'), 'Comparison').click();

    expect(log).toEqual([{ type: 'scope-change', detail: 'comparison' }]);
  });

  it('does not emit for the scope that is already current', async () => {
    const host = await mountBar({ scope: 'source' });
    const log = record(host);

    segment(required<ApmSegmented>(host, 'apm-segmented[label="Apply to"]'), 'Source').click();

    expect(log).toEqual([]);
  });

  it('stays controlled: it shows the property until the host feeds the change back', async () => {
    const host = await mountBar({ scope: 'both' });
    const selector = required<ApmSegmented>(host, 'apm-segmented[label="Apply to"]');

    segment(selector, 'Source').click();
    await rendered(host);
    expect(segment(selector, 'Both').getAttribute('aria-checked')).toBe('true');
    expect(segment(selector, 'Source').getAttribute('aria-checked')).toBe('false');

    await feed(host, { scope: 'source' });
    expect(segment(selector, 'Source').getAttribute('aria-checked')).toBe('true');
  });

  it('ignores a scope it does not know', async () => {
    const host = await mountBar({ scope: 'both' });
    const log = record(host);

    send(required(host, 'apm-segmented[label="Apply to"]'), 'segmented-change', 'bogus');

    expect(log).toEqual([]);
  });

  it('keeps its own event inside: only scope-change leaves the bar', async () => {
    const host = await mountBar({ scope: 'both' });
    const leaked = vi.fn();
    listen(document.body, 'segmented-change', leaked);

    segment(required<ApmSegmented>(host, 'apm-segmented[label="Apply to"]'), 'Source').click();

    expect(leaked).not.toHaveBeenCalled();
  });
});

describe('apm-filter-bar filter events', () => {
  it('emits the new filter when a rating is chosen', async () => {
    const host = await mountBar();
    const log = record(host);

    segment(ratingOf(host), '4 stars').click();

    expect(log).toEqual([{ type: 'filter-change', detail: filterWith({ ratings: [4] }) }]);
  });

  it('adds a rating to the ones already chosen, in ascending order, and removes one clicked again', async () => {
    const host = await mountBar({ filter: filterWith({ ratings: [5] }) });
    const log = record(host);

    segment(ratingOf(host), '2 stars').click();
    segment(ratingOf(host), '5 stars').click();

    expect(log).toEqual([
      { type: 'filter-change', detail: filterWith({ ratings: [2, 5] }) },
      { type: 'filter-change', detail: filterWith({ ratings: [] }) },
    ]);
  });

  it('sends 0 for the unrated', async () => {
    const host = await mountBar();
    const log = record(host);

    segment(ratingOf(host), 'Unrated').click();

    expect(log).toEqual([{ type: 'filter-change', detail: filterWith({ ratings: [0] }) }]);
  });

  it('emits the flags in a fixed order whatever the order of the clicks', async () => {
    const host = await mountBar({ filter: filterWith({ flags: ['rejected'] }) });
    const log = record(host);

    segment(flagsOf(host), 'Selected').click();

    expect(log).toEqual([
      { type: 'filter-change', detail: filterWith({ flags: ['selected', 'rejected'] }) },
    ]);
  });

  it('keeps the rest of the filter, the selection included', async () => {
    const filter = filterWith({
      cameras: ['Sony A7 IV'],
      ratings: [1],
      tags: ['sunset'],
      selection_id: 7,
    });
    const host = await mountBar({ filter });
    const log = record(host);

    segment(flagsOf(host), 'Selected').click();

    expect(log).toEqual([{ type: 'filter-change', detail: { ...filter, flags: ['selected'] } }]);
  });

  it('normalises what it emits', async () => {
    const host = await mountBar({
      filter: {
        cameras: [],
        ratings: [5, 3, 3],
        flags: ['rejected', 'selected'],
        tags: [' b', 'A', 'a'],
        selection_id: null,
      },
    });
    const log = record(host);

    segment(ratingOf(host), '1 star').click();

    expect(log).toEqual([
      {
        type: 'filter-change',
        detail: filterWith({
          ratings: [1, 3, 5],
          flags: ['selected', 'rejected'],
          tags: ['A', 'b'],
        }),
      },
    ]);
  });

  it('leaves the filter it was given alone: the host owns it and feeds the change back', async () => {
    const filter = filterWith({ ratings: [2] });
    const host = await mountBar({ filter });

    segment(ratingOf(host), '4 stars').click();
    await rendered(host);

    expect(host.filter).toBe(filter);
    expect(filter.ratings).toEqual([2]);
    expect(segment(ratingOf(host), '4 stars').getAttribute('aria-pressed')).toBe('false');
    expect(segment(ratingOf(host), '2 stars').getAttribute('aria-pressed')).toBe('true');
  });

  it('emits a tag that was typed, never while it is being typed', async () => {
    const host = await mountBar();
    const log = record(host);
    await openPanel(tagsOf(host));

    await typeIn(tagsOf(host), 'sunset');
    expect(log).toEqual([]);
    required(tagsOf(host), 'input[type="search"]').dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }),
    );

    expect(log).toEqual([{ type: 'filter-change', detail: filterWith({ tags: ['sunset'] }) }]);
  });

  it('removes a chosen value from its chip', async () => {
    const host = await mountBar({ filter: filterWith({ tags: ['family', 'sunset'] }) });
    const log = record(host);

    chipRemove(tagsOf(host), 'family').click();

    expect(log).toEqual([{ type: 'filter-change', detail: filterWith({ tags: ['sunset'] }) }]);
  });

  it('lets only its own events out, with the value as detail and bubbling', async () => {
    const host = await mountBar({ scope: 'both', filter: filterWith({ cameras: ['Sony A7 IV'] }) });
    const leaked: Array<string | null> = [];
    const heard = vi.fn();
    const noteLeak = (event: Event) => {
      leaked.push((event.target as Element).getAttribute('label'));
    };
    listen(document.body, 'segmented-change', noteLeak);
    listen(document.body, 'multi-select-change', noteLeak);
    listen(document.body, 'filter-change', heard);
    listen(document.body, 'scope-change', heard);

    segment(ratingOf(host), '3 stars').click();
    segment(flagsOf(host), 'Rejected').click();
    segment(required<ApmSegmented>(host, 'apm-segmented[label="Apply to"]'), 'Source').click();
    chipRemove(camerasOf(host), 'Sony A7 IV').click();
    await openPanel(tagsOf(host));
    await typeIn(tagsOf(host), 'x');
    required(tagsOf(host), 'input[type="search"]').dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }),
    );

    expect(leaked).toEqual([]);
    expect(heard).toHaveBeenCalledTimes(5);
  });
});

describe('apm-filter-bar clear filters', () => {
  it('is disabled while nothing is filtered', async () => {
    const host = await mountBar();

    expect(clearButton(host).disabled).toBe(true);
    expect(clearButton(host).hasAttribute('aria-label')).toBe(false);
    expect(clearButton(host).querySelector('.badge')).toBeNull();
  });

  it('says how many kinds of filter are active', async () => {
    const host = await mountBar({
      filter: filterWith({
        cameras: ['Sony A7 IV', 'Canon EOS R5'],
        ratings: [1],
        flags: ['rejected'],
        tags: ['sunset'],
      }),
    });

    expect(clearButton(host).disabled).toBe(false);
    expect(clearButton(host).getAttribute('aria-label')).toBe('Clear filters (4 active)');
    expect(clearButton(host).querySelector('.badge')?.textContent).toBe('4');
  });

  it('empties the filter with a single event', async () => {
    const host = await mountBar({ filter: filterWith({ ratings: [1, 2], tags: ['sunset'] }) });
    const log = record(host);

    clearButton(host).click();

    expect(log).toEqual([{ type: 'filter-change', detail: filterWith() }]);
  });

  it('drops the selection too: the filter first, then selection-clear', async () => {
    const host = await mountBar({ filter: filterWith({ ratings: [1], selection_id: 4 }) });
    const log = record(host);

    clearButton(host).click();

    expect(log).toEqual([
      { type: 'filter-change', detail: filterWith() },
      { type: 'selection-clear', detail: null },
    ]);
  });

  it('is available for a selection alone', async () => {
    const host = await mountBar({ filter: filterWith({ selection_id: 4 }) });
    const log = record(host);

    expect(clearButton(host).disabled).toBe(false);
    expect(clearButton(host).getAttribute('aria-label')).toBe('Clear filters (1 active)');
    clearButton(host).click();

    expect(log.map((entry) => entry.type)).toEqual(['filter-change', 'selection-clear']);
  });

  it('does nothing while the bar is disabled', async () => {
    const host = await mountBar({ disabled: true, filter: filterWith({ ratings: [1] }) });
    const log = record(host);

    expect(clearButton(host).disabled).toBe(true);
    clearButton(host).click();

    expect(log).toEqual([]);
  });
});

describe('apm-filter-bar selection chip', () => {
  it('is hidden without a selection, even when a label is given', async () => {
    const host = await mountBar({ selectionLabel: 'Duplicates of IMG_0001' });

    expect(selectionChip(host)).toBeNull();
    expect(host.textContent).not.toContain('Duplicates of IMG_0001');
  });

  it('shows the label of the selection', async () => {
    const host = await mountBar({
      filter: filterWith({ selection_id: 3 }),
      selectionLabel: 'Duplicates of IMG_0001',
    });

    const chip = required(host, '.badge-info');
    expect(chip.textContent).toContain('Duplicates of IMG_0001');
    expect(required(chip, '.truncate').getAttribute('title')).toBe('Duplicates of IMG_0001');
  });

  it.each([null, ''])('falls back to "Selection" when the label is %j', async (label) => {
    const host = await mountBar({ filter: filterWith({ selection_id: 3 }), selectionLabel: label });

    expect(required(host, '.badge-info').textContent).toContain('Selection');
  });

  it('shows and hides with the selection', async () => {
    const host = await mountBar({ filter: filterWith({ selection_id: 3 }) });
    expect(selectionChip(host)).not.toBeNull();

    await feed(host, { filter: filterWith() });

    expect(selectionChip(host)).toBeNull();
  });

  it('is removed with its button: selection-clear and nothing else', async () => {
    const filter = filterWith({ ratings: [1], selection_id: 3 });
    const host = await mountBar({ filter });
    const log = record(host);

    selectionRemove(host).click();

    expect(log).toEqual([{ type: 'selection-clear', detail: null }]);
    expect(host.filter).toBe(filter);
  });

  it('counts as one active filter', async () => {
    const host = await mountBar({ filter: filterWith({ ratings: [1], selection_id: 3 }) });

    expect(clearButton(host).getAttribute('aria-label')).toBe('Clear filters (2 active)');
  });

  it('cannot be removed while the bar is disabled', async () => {
    const host = await mountBar({ disabled: true, filter: filterWith({ selection_id: 3 }) });
    const log = record(host);

    expect(selectionRemove(host).disabled).toBe(true);
    selectionRemove(host).click();

    expect(log).toEqual([]);
  });
});

describe('apm-filter-bar hide-unmatched toggle', () => {
  it('shows its state and what it does', async () => {
    const host = await mountBar({ hideUnmatched: true });

    const input = toggle(host);
    expect(input.checked).toBe(true);
    expect(input.getAttribute('role')).toBe('switch');
    expect(input.classList.contains('toggle')).toBe(true);
    const label = required(host, 'label');
    expect(label.contains(input)).toBe(true);
    expect(label.textContent).toContain('Hide folders without matches');
    expect(label.getAttribute('title')).toBe('Folders without matching files are hidden');

    await feed(host, { hideUnmatched: false });

    expect(input.checked).toBe(false);
    expect(label.getAttribute('title')).toBe('Folders without matching files are greyed out');
  });

  it('reads the hide-unmatched attribute', async () => {
    const host = await mountBar();

    host.setAttribute('hide-unmatched', '');
    await rendered(host);

    expect(host.hideUnmatched).toBe(true);
    expect(toggle(host).checked).toBe(true);
  });

  it('emits the new state, true then false', async () => {
    const host = await mountBar({ hideUnmatched: false });
    const log = record(host);

    toggle(host).click();
    await feed(host, { hideUnmatched: true });
    toggle(host).click();

    expect(log).toEqual([
      { type: 'hide-unmatched-change', detail: true },
      { type: 'hide-unmatched-change', detail: false },
    ]);
  });

  it('stays controlled: the switch shows the property until the host feeds the change back', async () => {
    const host = await mountBar({ hideUnmatched: false });

    toggle(host).click();
    await rendered(host);
    expect(toggle(host).checked).toBe(false);

    await feed(host, { hideUnmatched: true });
    expect(toggle(host).checked).toBe(true);
  });

  it('is locked while the bar is disabled, even against a change that still arrives', async () => {
    const host = await mountBar({ disabled: true, hideUnmatched: false });
    const log = record(host);
    const input = toggle(host);

    expect(input.disabled).toBe(true);
    expect(required(host, 'label').classList.contains('cursor-not-allowed')).toBe(true);
    input.checked = true;
    input.dispatchEvent(new Event('change', { bubbles: true }));

    expect(log).toEqual([]);
    expect(input.checked).toBe(false);
  });

  it('does not change the filter', async () => {
    const host = await mountBar();
    const log = record(host);

    toggle(host).click();

    expect(log.map((entry) => entry.type)).toEqual(['hide-unmatched-change']);
  });
});

describe('apm-filter-bar disabled', () => {
  it('locks every control', async () => {
    const host = await mountBar({
      disabled: true,
      scope: 'both',
      filter: filterWith({ ratings: [1], selection_id: 2 }),
    });

    expect(camerasOf(host).disabled).toBe(true);
    expect(tagsOf(host).disabled).toBe(true);
    for (const group of [
      ratingOf(host),
      flagsOf(host),
      required<ApmSegmented>(host, 'apm-segmented[label="Apply to"]'),
    ]) {
      expect(group.disabled).toBe(true);
      for (const button of Array.from(group.querySelectorAll('button'))) {
        expect(button.disabled).toBe(true);
      }
    }
    expect(clearButton(host).disabled).toBe(true);
    expect(selectionRemove(host).disabled).toBe(true);
    expect(toggle(host).disabled).toBe(true);
    await openPanel(camerasOf(host));
    expect(host.querySelector('input[type="search"]')).toBeNull();
  });

  it('blocks every event, even one a control still sends', async () => {
    const host = await mountBar({ disabled: true, scope: 'both' });
    const log = record(host);

    send(ratingOf(host), 'segmented-change', ['3']);
    send(flagsOf(host), 'segmented-change', ['selected']);
    send(required(host, 'apm-segmented[label="Apply to"]'), 'segmented-change', 'comparison');
    send(camerasOf(host), 'multi-select-change', ['Sony A7 IV']);
    send(tagsOf(host), 'multi-select-change', ['sunset']);
    toggle(host).dispatchEvent(new Event('change', { bubbles: true }));

    expect(log).toEqual([]);
  });

  it('ignores a click that still reaches a locked button', async () => {
    const host = await mountBar({
      disabled: true,
      filter: filterWith({ tags: ['sunset'], selection_id: 7 }),
    });
    const log = record(host);

    clearButton(host).dispatchEvent(new MouseEvent('click', { bubbles: true }));
    selectionRemove(host).dispatchEvent(new MouseEvent('click', { bubbles: true }));

    expect(log).toEqual([]);
  });

  it('shows why and ties the explanation to the group', async () => {
    const host = await mountBar({ disabled: true, disabledReason: 'Plans take no filter' });

    const reason = required(host, 'p[id$="-reason"]');
    expect(reason.textContent).toBe('Plans take no filter');
    expect(required(host, 'div[role="group"]').getAttribute('aria-describedby')).toBe(reason.id);
  });

  it('says nothing without a reason, and nothing while enabled', async () => {
    const host = await mountBar({ disabled: true });
    expect(host.querySelector('p')).toBeNull();
    expect(required(host, 'div[role="group"]').hasAttribute('aria-describedby')).toBe(false);

    await feed(host, { disabled: false, disabledReason: 'Plans take no filter' });

    expect(host.querySelector('p')).toBeNull();
    expect(required(host, 'div[role="group"]').hasAttribute('aria-describedby')).toBe(false);
  });

  it('asks for nothing while disabled', async () => {
    const calls = fakeFacetApi();
    await mountAt([TRIPS], { disabled: true });

    await advance(5000);

    expect(calls).toHaveLength(0);
  });

  it('drops the values it had and cancels the request in flight', async () => {
    const calls = fakeFacetApi();
    const host = await mountLoaded(calls);
    expect(camerasOf(host).options).toHaveLength(2);
    await feed(host, { filter: filterWith({ ratings: [1] }) });
    await advance(250);
    const inFlight = nth(calls, 1);

    await feed(host, { disabled: true });

    expect(inFlight.signal?.aborted).toBe(true);
    expect(camerasOf(host).options).toEqual([]);
    expect(tagsOf(host).options).toEqual([]);
    expect(camerasOf(host).loading).toBe(false);
    await advance(5000);
    expect(calls).toHaveLength(2);
  });

  it('asks again once it is enabled again', async () => {
    const calls = fakeFacetApi();
    const host = await mountAt([TRIPS], { disabled: true });

    await feed(host, { disabled: false });
    await advance(250);

    expect(calls).toHaveLength(1);
  });
});

describe('apm-filter-bar facet requests', () => {
  it('asks for nothing without a location', async () => {
    const calls = fakeFacetApi();
    await mountAt([]);

    await advance(5000);

    expect(calls).toHaveLength(0);
  });

  it('asks for the values of its location 250 ms after it got it', async () => {
    const calls = fakeFacetApi();
    await mountAt([TRIPS]);

    await advance(249);
    expect(calls).toHaveLength(0);
    await advance(1);

    expect(calls).toHaveLength(1);
    expect(nth(calls, 0).path).toBe('/api/facets');
    expect(nth(calls, 0).body).toEqual({
      volume_id: 1,
      dir: 'trips',
      filter: EMPTY_FILTER,
      plan: null,
    });
    expect(nth(calls, 0).signal).toBeInstanceOf(AbortSignal);
  });

  it('also asks for a bar that is mounted with its location', async () => {
    const calls = fakeFacetApi();
    vi.useFakeTimers();

    await mountBar({ contexts: [TRIPS] });
    await advance(250);

    expect(calls).toHaveLength(1);
  });

  it('sends the location and its own filter, nothing a pane may carry besides', async () => {
    const calls = fakeFacetApi();
    const pane = {
      ...where(1, 'trips'),
      filter: filterWith({ cameras: ['Not this one'] }),
      plan: { plan_id: 3, side: 'source' },
      extra: true,
    } as BrowseRequest;
    await mountAt([pane], { filter: filterWith({ ratings: [5, 3, 3], tags: [' b', 'A'] }) });

    await advance(250);

    const { body } = nth(calls, 0);
    expect(Object.keys(body).sort()).toEqual(['dir', 'filter', 'plan', 'volume_id']);
    expect(body.plan).toBeNull();
    expect(body.filter).toEqual(filterWith({ ratings: [3, 5], tags: ['A', 'b'] }));
  });

  it('asks for the whole library with a null volume and the root', async () => {
    const calls = fakeFacetApi();
    await mountAt([where(null, '')]);

    await advance(250);

    expect(nth(calls, 0).body).toMatchObject({ volume_id: null, dir: '' });
  });

  it('fills the lists with the answer, in the order of the server and with the counts', async () => {
    const calls = fakeFacetApi();
    const host = await mountLoaded(calls);

    expect(camerasOf(host).options).toEqual([
      { value: 'Sony A7 IV', count: 1200 },
      { value: 'Canon EOS R5', count: 340 },
    ]);
    expect(tagsOf(host).options).toEqual([
      { value: 'sunset', count: 30 },
      { value: 'family', count: 12 },
    ]);
  });

  it('lists the values in the dropdowns, with their counts', async () => {
    const calls = fakeFacetApi();
    const host = await mountLoaded(calls);

    await openPanel(camerasOf(host));

    expect(rowNames(camerasOf(host))).toEqual(['Sony A7 IV', 'Canon EOS R5']);
    expect(camerasOf(host).textContent).toContain(formatCount(1200));
    expect(camerasOf(host).textContent).toContain(formatCount(340));
  });

  it('emits the camera chosen in the dropdown', async () => {
    const calls = fakeFacetApi();
    const host = await mountLoaded(calls);
    const log = record(host);

    await pick(camerasOf(host), 'Canon EOS R5');

    expect(log).toEqual([
      { type: 'filter-change', detail: filterWith({ cameras: ['Canon EOS R5'] }) },
    ]);
  });

  it('puts the number of items on each rating, 0 for a rating nobody gave', async () => {
    const calls = fakeFacetApi();
    const host = await mountLoaded(calls);

    expect(segmentNames(ratingOf(host))).toEqual([
      'Unrated (0)',
      '1 star (0)',
      '2 stars (0)',
      '3 stars (2)',
      '4 stars (0)',
      '5 stars (7)',
    ]);
  });

  it('shows no counts before the answer', async () => {
    const calls = fakeFacetApi();
    const host = await mountAt([TRIPS]);
    await advance(250);

    expect(calls).toHaveLength(1);
    expect(ratingOf(host).options.every((option) => option.count === undefined)).toBe(true);
    expect(camerasOf(host).options).toEqual([]);
  });

  it('tells the lists it is loading until the answer comes', async () => {
    const calls = fakeFacetApi();
    const host = await mountAt([TRIPS]);
    expect(camerasOf(host).loading).toBe(false);

    await advance(250);
    expect(camerasOf(host).loading).toBe(true);
    expect(tagsOf(host).loading).toBe(true);

    await answer(nth(calls, 0), FACETS);
    expect(camerasOf(host).loading).toBe(false);
    expect(tagsOf(host).loading).toBe(false);
  });

  it('waits for the filter to settle: several changes, one request, the last filter', async () => {
    const calls = fakeFacetApi();
    const host = await mountLoaded(calls);

    await feed(host, { filter: filterWith({ ratings: [4] }) });
    await advance(100);
    await feed(host, { filter: filterWith({ ratings: [4, 5] }) });
    await advance(249);
    expect(calls).toHaveLength(1);
    await advance(1);

    expect(calls).toHaveLength(2);
    expect(nth(calls, 1).body.filter).toEqual(filterWith({ ratings: [4, 5] }));
  });

  it('sends every kind of filter, the selection included', async () => {
    const calls = fakeFacetApi();
    const host = await mountLoaded(calls);

    await feed(host, {
      filter: filterWith({
        cameras: ['Sony A7 IV'],
        ratings: [4],
        flags: ['selected'],
        tags: ['sunset'],
        selection_id: 5,
      }),
    });
    await advance(250);

    expect(nth(calls, 1).body.filter).toEqual({
      cameras: ['Sony A7 IV'],
      ratings: [4],
      flags: ['selected'],
      tags: ['sunset'],
      selection_id: 5,
    });
  });

  it('cancels the request in flight when the filter changes', async () => {
    const calls = fakeFacetApi();
    const host = await mountAt([TRIPS]);
    await advance(250);
    const first = nth(calls, 0);
    expect(first.signal?.aborted).toBe(false);

    await feed(host, { filter: filterWith({ ratings: [1] }) });

    expect(first.signal?.aborted).toBe(true);
    await advance(250);
    expect(calls).toHaveLength(2);
    expect(nth(calls, 1).signal?.aborted).toBe(false);
  });

  it('shows no failure for a request it cancelled itself', async () => {
    const calls = fakeFacetApi();
    const host = await mountAt([TRIPS]);
    await advance(250);

    await feed(host, { filter: filterWith({ ratings: [1] }) });
    await flush();

    expect(nth(calls, 0).signal?.aborted).toBe(true);
    expect(failureHint(host)).toBeNull();
  });

  it('drops the late answer of a request it had cancelled', async () => {
    const calls = fakeFacetApi({ honourAbort: false });
    const host = await mountAt([TRIPS]);
    await advance(250);
    await feed(host, { filter: filterWith({ ratings: [1] }) });
    await advance(250);
    expect(calls).toHaveLength(2);

    await answer(nth(calls, 1), OTHER_FACETS);
    await answer(nth(calls, 0), FACETS);

    expect(camerasOf(host).options).toEqual(OTHER_FACETS.cameras);
    expect(tagsOf(host).options).toEqual(OTHER_FACETS.tags);
    expect(camerasOf(host).loading).toBe(false);
  });

  it('drops a late answer even while the current request is still waiting', async () => {
    const calls = fakeFacetApi({ honourAbort: false });
    const host = await mountAt([TRIPS]);
    await advance(250);
    await feed(host, { filter: filterWith({ ratings: [1] }) });
    await advance(250);

    await answer(nth(calls, 0), FACETS);

    expect(camerasOf(host).options).toEqual([]);
    expect(camerasOf(host).loading).toBe(true);
    await answer(nth(calls, 1), OTHER_FACETS);
    expect(camerasOf(host).options).toEqual(OTHER_FACETS.cameras);
    expect(camerasOf(host).loading).toBe(false);
  });

  it('ignores the late failure of a request it had cancelled', async () => {
    const calls = fakeFacetApi({ honourAbort: false });
    const host = await mountAt([TRIPS]);
    await advance(250);
    await feed(host, { filter: filterWith({ ratings: [1] }) });
    await advance(250);
    expect(calls).toHaveLength(2);

    await fail(nth(calls, 0));

    expect(failureHint(host)).toBeNull();
    expect(camerasOf(host).loading).toBe(true);
    await answer(nth(calls, 1), OTHER_FACETS);
    expect(camerasOf(host).options).toEqual(OTHER_FACETS.cameras);
    expect(failureHint(host)).toBeNull();
  });

  it('keeps the values it has while the next ones load', async () => {
    const calls = fakeFacetApi();
    const host = await mountLoaded(calls);

    await feed(host, { contexts: [BACKUP] });
    expect(camerasOf(host).options).toHaveLength(2);
    await advance(250);
    expect(camerasOf(host).options).toHaveLength(2);
    expect(camerasOf(host).loading).toBe(true);
    expect(nth(calls, 1).body).toMatchObject({ volume_id: 2, dir: '' });

    await answer(nth(calls, 1), OTHER_FACETS);
    expect(camerasOf(host).options).toEqual(OTHER_FACETS.cameras);
  });

  it('does not ask again when only the identity of its contexts changed', async () => {
    const calls = fakeFacetApi();
    const host = await mountLoaded(calls);

    await feed(host, { contexts: [{ ...TRIPS }] });
    await feed(host, { contexts: [{ ...TRIPS, filter: filterWith({ ratings: [1] }) }] });
    await feed(host, {
      contexts: [{ ...TRIPS, plan: { plan_id: 1, side: 'source' } }],
    });
    await advance(5000);

    expect(calls).toHaveLength(1);
  });

  it('does not ask again for a filter that is the same in value', async () => {
    const calls = fakeFacetApi();
    const host = await mountLoaded(calls, FACETS, { filter: filterWith({ tags: ['a'] }) });
    await advance(250);
    expect(calls).toHaveLength(1);

    await feed(host, { filter: filterWith({ tags: ['A '] }) });
    await feed(host, { filter: { ...filterWith({ tags: ['a'] }), ratings: [] } });
    await advance(5000);

    expect(calls).toHaveLength(1);
  });

  it('does not ask again when something the answer does not depend on changes', async () => {
    const calls = fakeFacetApi();
    const host = await mountLoaded(calls);

    await feed(host, {
      hideUnmatched: true,
      selectionLabel: 'A selection',
      scope: 'source',
      scopeLabels: { both: 'A', source: 'B', comparison: 'C' },
      disabledReason: 'Unused',
    });
    await advance(5000);

    expect(calls).toHaveLength(1);
  });

  it('asks again when the location changes', async () => {
    const calls = fakeFacetApi();
    const host = await mountLoaded(calls);

    await feed(host, { contexts: [where(1, 'trips/2024')] });
    await advance(250);

    expect(calls).toHaveLength(2);
    expect(nth(calls, 1).body).toMatchObject({ volume_id: 1, dir: 'trips/2024' });
  });

  it('asks once per distinct location and merges the answers: the union, without counts', async () => {
    const calls = fakeFacetApi();
    const host = await mountAt([TRIPS, BACKUP]);

    await advance(250);
    expect(calls).toHaveLength(2);
    expect(nth(calls, 0).body).toMatchObject({ volume_id: 1, dir: 'trips' });
    expect(nth(calls, 1).body).toMatchObject({ volume_id: 2, dir: '' });
    await answer(nth(calls, 1), SECOND_PANE);
    await answer(nth(calls, 0), FIRST_PANE);

    expect(camerasOf(host).options).toEqual([
      { value: 'Canon EOS R5' },
      { value: 'Fujifilm X-T4' },
      { value: 'Sony A7 IV' },
    ]);
    expect(tagsOf(host).options).toEqual([{ value: 'family' }, { value: 'sunset' }]);
    expect(segmentNames(ratingOf(host))).toEqual([
      'Unrated',
      '1 star',
      '2 stars',
      '3 stars',
      '4 stars',
      '5 stars',
    ]);
  });

  it('shows nothing of a pair until both answers are in', async () => {
    const calls = fakeFacetApi();
    const host = await mountAt([TRIPS, BACKUP]);
    await advance(250);

    await answer(nth(calls, 0), FIRST_PANE);

    expect(camerasOf(host).options).toEqual([]);
    expect(camerasOf(host).loading).toBe(true);
    await answer(nth(calls, 1), SECOND_PANE);
    expect(camerasOf(host).options).toHaveLength(3);
    expect(camerasOf(host).loading).toBe(false);
  });

  it('asks once for two panes on the same folder and keeps the counts', async () => {
    const calls = fakeFacetApi();
    const host = await mountAt([TRIPS, { ...TRIPS }]);

    await advance(250);
    expect(calls).toHaveLength(1);
    await answer(nth(calls, 0), FACETS);

    expect(camerasOf(host).options).toEqual(FACETS.cameras);
    expect(segmentNames(ratingOf(host))).toContain('5 stars (7)');
  });
});

describe('apm-filter-bar refreshing', () => {
  it('asks again at once when the event stream reconnects', async () => {
    vi.useFakeTimers();
    connectEvents();
    const calls = fakeFacetApi();
    const host = await mountBar({ contexts: [TRIPS] });
    await advance(250);
    await answer(nth(calls, 0), FACETS);

    await reconnectEvents();

    expect(calls).toHaveLength(2);
    await answer(nth(calls, 1), OTHER_FACETS);
    expect(camerasOf(host).options).toEqual(OTHER_FACETS.cameras);
  });

  it.each(['done', 'failed', 'cancelled', 'interrupted'] as const)(
    'asks again at once when an index task is %s',
    async (state) => {
      const calls = fakeFacetApi();
      await mountLoaded(calls);

      events.emit(taskEvent(state));

      expect(calls).toHaveLength(2);
    },
  );

  it.each(['queued', 'running'] as const)(
    'does not ask while an index task is %s',
    async (state) => {
      const calls = fakeFacetApi();
      await mountLoaded(calls);

      events.emit(taskEvent(state));
      await advance(5000);

      expect(calls).toHaveLength(1);
    },
  );

  it('ignores the end of a task that is not an index', async () => {
    const calls = fakeFacetApi();
    await mountLoaded(calls);

    events.emit(taskEvent('done', 'fs_move'));
    events.emit(taskEvent('done', 'plan_execute'));
    await advance(5000);

    expect(calls).toHaveLength(1);
  });

  it('cancels the request in flight when it asks again', async () => {
    const calls = fakeFacetApi();
    const host = await mountAt([TRIPS]);
    await advance(250);
    const first = nth(calls, 0);

    events.emit(taskEvent('done'));
    await flush();

    expect(first.signal?.aborted).toBe(true);
    expect(calls).toHaveLength(2);
    await answer(nth(calls, 1), FACETS);
    expect(camerasOf(host).options).toEqual(FACETS.cameras);
  });

  it('asks again 2 s after a change below its folder', async () => {
    const calls = fakeFacetApi();
    await mountLoaded(calls);

    events.emit(fsChange(1, 'trips/2024/may'));
    await advance(1999);
    expect(calls).toHaveLength(1);
    await advance(1);

    expect(calls).toHaveLength(2);
  });

  it('reacts to its own folder, and to any change when it covers the library', async () => {
    const calls = fakeFacetApi();
    await mountLoaded(calls);
    events.emit(fsChange(1, 'trips'));
    await advance(2000);
    expect(calls).toHaveLength(2);

    const everything = fakeFacetApi();
    await mountAt([where(null, '')]);
    await advance(250);
    events.emit(fsChange(7, 'anything/at/all'));
    await advance(2000);

    expect(everything).toHaveLength(2);
  });

  it('ignores changes elsewhere', async () => {
    const calls = fakeFacetApi();
    await mountLoaded(calls);

    events.emit(fsChange(2, 'trips'));
    events.emit(fsChange(1, 'tripsy'));
    events.emit(fsChange(1, 'other', 'elsewhere/trips'));
    await advance(30_000);

    expect(calls).toHaveLength(1);
  });

  it('waits for a burst to end', async () => {
    const calls = fakeFacetApi();
    await mountLoaded(calls);

    events.emit(fsChange(1, 'trips'));
    await advance(1500);
    events.emit(fsChange(1, 'trips'));
    await advance(1500);
    events.emit(fsChange(1, 'trips'));
    await advance(1999);
    expect(calls).toHaveLength(1);
    await advance(1);

    expect(calls).toHaveLength(2);
  });

  it('never waits longer than 10 s for a burst that does not end', async () => {
    const calls = fakeFacetApi();
    await mountLoaded(calls);

    for (let elapsed = 0; elapsed <= 9000; elapsed += 1500) {
      events.emit(fsChange(1, 'trips'));
      if (elapsed < 9000) await advance(1500);
    }
    expect(calls).toHaveLength(1);
    await advance(999);
    expect(calls).toHaveLength(1);
    await advance(1);

    expect(calls).toHaveLength(2);
  });

  it('starts a new wait for the next burst', async () => {
    const calls = fakeFacetApi();
    await mountLoaded(calls);
    events.emit(fsChange(1, 'trips'));
    await advance(2000);
    expect(calls).toHaveLength(2);

    events.emit(fsChange(1, 'trips'));
    await advance(1999);
    expect(calls).toHaveLength(2);
    await advance(1);

    expect(calls).toHaveLength(3);
  });

  it('asks once when a filter change and a file change come together', async () => {
    const calls = fakeFacetApi();
    const host = await mountLoaded(calls);

    await feed(host, { filter: filterWith({ ratings: [1] }) });
    events.emit(fsChange(1, 'trips'));
    await advance(10_000);

    expect(calls).toHaveLength(2);
  });

  it('ignores file changes while disabled', async () => {
    const calls = fakeFacetApi();
    await mountAt([TRIPS], { disabled: true });

    events.emit(fsChange(1, 'trips'));
    events.emit(taskEvent('done'));
    await advance(30_000);

    expect(calls).toHaveLength(0);
  });

  it('stops listening and asking when it leaves the page', async () => {
    const calls = fakeFacetApi();
    vi.useFakeTimers();
    connectEvents();
    const host = await mountBar({ contexts: [TRIPS] });
    await advance(250);
    await answer(nth(calls, 0), FACETS);
    await feed(host, { filter: filterWith({ ratings: [1] }) });
    events.emit(fsChange(1, 'trips'));

    host.remove();
    events.emit(taskEvent('done'));
    events.emit(fsChange(1, 'trips'));
    await reconnectEvents();
    await advance(30_000);

    expect(calls).toHaveLength(1);
  });

  it('cancels the request in flight when it leaves the page', async () => {
    const calls = fakeFacetApi();
    const host = await mountAt([TRIPS]);
    await advance(250);

    host.remove();

    expect(nth(calls, 0).signal?.aborted).toBe(true);
  });

  it('starts again when it comes back', async () => {
    const calls = fakeFacetApi();
    const host = await mountLoaded(calls);
    host.remove();
    await advance(5000);
    expect(calls).toHaveLength(1);

    document.body.append(host);
    await rendered(host);
    await advance(250);
    expect(calls).toHaveLength(2);
    await answer(nth(calls, 1), OTHER_FACETS);
    events.emit(taskEvent('done'));

    expect(calls).toHaveLength(3);
    expect(camerasOf(host).options).toEqual(OTHER_FACETS.cameras);
  });
});

describe('apm-filter-bar failures', () => {
  it('keeps the values it has, says the options are unavailable and stays usable', async () => {
    const calls = fakeFacetApi();
    const host = await mountLoaded(calls);
    events.emit(taskEvent('done'));

    await fail(nth(calls, 1));

    expect(camerasOf(host).options).toEqual(FACETS.cameras);
    expect(tagsOf(host).options).toEqual(FACETS.tags);
    expect(camerasOf(host).loading).toBe(false);
    const hint = failureHint(host);
    expect(hint?.textContent).toContain('Filter options unavailable');
    expect(hint?.getAttribute('title')).toBe('The server is down');
    const log = record(host);
    segment(ratingOf(host), '2 stars').click();
    expect(log).toEqual([{ type: 'filter-change', detail: filterWith({ ratings: [2] }) }]);
  });

  it('leaves the lists empty after a first failure, and the controls working', async () => {
    const calls = fakeFacetApi();
    const host = await mountAt([TRIPS]);
    await advance(250);

    await fail(nth(calls, 0));

    expect(camerasOf(host).options).toEqual([]);
    expect(failureHint(host)).not.toBeNull();
    expect(camerasOf(host).loading).toBe(false);
    const log = record(host);
    await openPanel(tagsOf(host));
    await typeIn(tagsOf(host), 'sunset');
    required(tagsOf(host), 'input[type="search"]').dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }),
    );
    expect(log).toEqual([{ type: 'filter-change', detail: filterWith({ tags: ['sunset'] }) }]);
  });

  it('uses a plain message when the failure has none', async () => {
    const calls = fakeFacetApi();
    const host = await mountAt([TRIPS]);
    await advance(250);

    await fail(nth(calls, 0), {});

    expect(failureHint(host)?.getAttribute('title')).toBe('Could not load the filter options');
  });

  it('offers a retry that asks at once and clears the hint with the answer', async () => {
    const calls = fakeFacetApi();
    const host = await mountLoaded(calls);
    events.emit(taskEvent('done'));
    await fail(nth(calls, 1));
    expect(retryButton(host).disabled).toBe(false);

    retryButton(host).click();
    await flush();

    expect(calls).toHaveLength(3);
    expect(retryButton(host).disabled).toBe(true);
    await answer(nth(calls, 2), OTHER_FACETS);
    expect(failureHint(host)).toBeNull();
    expect(camerasOf(host).options).toEqual(OTHER_FACETS.cameras);
  });

  it('lets the retry fail again, and offers it again', async () => {
    const calls = fakeFacetApi();
    const host = await mountAt([TRIPS]);
    await advance(250);
    await fail(nth(calls, 0));

    retryButton(host).click();
    await flush();
    await fail(nth(calls, 1));

    expect(failureHint(host)).not.toBeNull();
    expect(retryButton(host).disabled).toBe(false);
  });

  it('clears the hint when a new filter gets an answer', async () => {
    const calls = fakeFacetApi();
    const host = await mountAt([TRIPS]);
    await advance(250);
    await fail(nth(calls, 0));

    await feed(host, { filter: filterWith({ ratings: [1] }) });
    await advance(250);
    expect(failureHint(host)).not.toBeNull();
    await answer(nth(calls, 1), FACETS);

    expect(failureHint(host)).toBeNull();
  });

  it('is all or nothing with two locations: one failure cancels the other and keeps the old values', async () => {
    const calls = fakeFacetApi();
    const host = await mountAt([TRIPS, BACKUP]);
    await advance(250);
    expect(calls).toHaveLength(2);

    await fail(nth(calls, 1));

    expect(nth(calls, 0).signal?.aborted).toBe(true);
    expect(camerasOf(host).options).toEqual([]);
    expect(failureHint(host)).not.toBeNull();
    expect(camerasOf(host).loading).toBe(false);
  });

  it('does not show a half answer when the second location fails', async () => {
    const calls = fakeFacetApi();
    const host = await mountAt([TRIPS, BACKUP]);
    await advance(250);

    await answer(nth(calls, 0), FIRST_PANE);
    await fail(nth(calls, 1));

    expect(camerasOf(host).options).toEqual([]);
    expect(failureHint(host)).not.toBeNull();
  });

  it('ignores the failure of a request it had replaced', async () => {
    const calls = fakeFacetApi({ honourAbort: false });
    const host = await mountAt([TRIPS]);
    await advance(250);
    await feed(host, { filter: filterWith({ ratings: [1] }) });
    await advance(250);

    await fail(nth(calls, 0));

    expect(failureHint(host)).toBeNull();
    expect(camerasOf(host).loading).toBe(true);
  });

  it('hides the hint, not the reason, once the bar is disabled', async () => {
    const calls = fakeFacetApi();
    const host = await mountAt([TRIPS]);
    await advance(250);
    await fail(nth(calls, 0));
    expect(failureHint(host)).not.toBeNull();

    await feed(host, { disabled: true, disabledReason: 'No filter here' });

    expect(failureHint(host)).toBeNull();
    expect(required(host, 'p[id$="-reason"]').textContent).toBe('No filter here');
    await feed(host, { disabled: false });
    expect(failureHint(host)).toBeNull();
  });

  it('lists a chosen value the facets do not know, and lets it be removed', async () => {
    const calls = fakeFacetApi();
    const host = await mountLoaded(calls, FACETS, {
      filter: filterWith({ cameras: ['Leica Q2'] }),
    });
    const log = record(host);

    expect(chipsOf(camerasOf(host))).toEqual(['Leica Q2']);
    chipRemove(camerasOf(host), 'Leica Q2').click();

    expect(log).toEqual([{ type: 'filter-change', detail: filterWith({ cameras: [] }) }]);
  });

  it('keeps a chosen value removable before any value is known', async () => {
    const host = await mountBar({ filter: filterWith({ tags: ['sunset'] }) });
    const log = record(host);

    expect(chipsOf(tagsOf(host))).toEqual(['sunset']);
    chipRemove(tagsOf(host), 'sunset').click();

    expect(log).toEqual([{ type: 'filter-change', detail: filterWith() }]);
  });
});
