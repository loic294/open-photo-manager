import { html } from 'lit';
import { afterEach, describe, expect, it, vi } from 'vitest';
import '@/components/opm-bottom-bar';
import '@/components/opm-split-button';
import type { OpmBottomBar, PaneFooter } from '@/components/opm-bottom-bar';
import type { OpmSplitButton } from '@/components/opm-split-button';
import { mount } from '@/test/helpers';
import { formatCount } from '@/util/format-count';

type Pane = 'source' | 'comparison';

const LABELS: Record<Pane, string> = { source: 'Source pane', comparison: 'Comparison pane' };

function footer(overrides: Partial<PaneFooter> = {}): PaneFooter {
  return {
    counts: { in_folder: 12, in_subfolders: 340 },
    selectedCount: 0,
    selecting: false,
    active: false,
    size: 160,
    sizeMin: 96,
    sizeMax: 320,
    sizeStep: 16,
    ...overrides,
  };
}

afterEach(() => {
  vi.restoreAllMocks();
});

function segment(host: OpmBottomBar, pane: Pane): HTMLElement {
  const element = host.querySelector<HTMLElement>(`[role="group"][aria-label="${LABELS[pane]}"]`);
  if (!element) throw new Error(`no ${pane} segment`);
  return element;
}

function segments(host: OpmBottomBar): HTMLElement[] {
  return Array.from(host.querySelectorAll<HTMLElement>('[role="group"]'));
}

function slider(host: OpmBottomBar, pane: Pane): HTMLInputElement {
  const input = segment(host, pane).querySelector<HTMLInputElement>('input[type="range"]');
  if (!input) throw new Error('no slider');
  return input;
}

function selectAll(host: OpmBottomBar, pane: Pane): HTMLButtonElement {
  const button = segment(host, pane).querySelector<HTMLButtonElement>(
    'button[aria-label="Select all"]',
  );
  if (!button) throw new Error('no select-all button');
  return button;
}

function clearButton(host: OpmBottomBar, pane: Pane): HTMLButtonElement | null {
  return segment(host, pane).querySelector<HTMLButtonElement>(
    'button[aria-label="Clear selection"]',
  );
}

function countsOf(host: OpmBottomBar, pane: Pane): HTMLElement {
  const element = segment(host, pane).querySelector<HTMLElement>('[role="status"]:not(.sr-only)');
  if (!element) throw new Error('no counts');
  return element;
}

/** Moves a slider like a user does: the value changes, then an `input` event fires. */
function drag(input: HTMLInputElement, value: number): void {
  input.value = String(value);
  input.dispatchEvent(new Event('input', { bubbles: true }));
}

describe('opm-bottom-bar structure', () => {
  it('is a compact DaisyUI navbar with a top border', async () => {
    const host = await mount('opm-bottom-bar', { source: footer() });

    expect(host.classList.contains('block')).toBe(true);
    const bar = host.querySelector('.navbar');
    expect(bar).not.toBeNull();
    for (const name of ['border-t', 'border-base-300', 'bg-base-100']) {
      expect(bar?.classList.contains(name)).toBe(true);
    }
  });

  it('renders the source segment alone for a tool without comparison pane', async () => {
    const host = await mount('opm-bottom-bar', { source: footer() });

    expect(segments(host)).toHaveLength(1);
    expect(segments(host)[0]?.getAttribute('aria-label')).toBe('Source pane');
  });

  it('omits the comparison segment for null', async () => {
    const host = await mount('opm-bottom-bar', { source: footer(), comparison: null });

    expect(segments(host).map((element) => element.getAttribute('aria-label'))).toEqual([
      'Source pane',
    ]);
  });

  it('renders the source then the comparison segment', async () => {
    const host = await mount('opm-bottom-bar', { source: footer(), comparison: footer() });

    expect(segments(host).map((element) => element.getAttribute('aria-label'))).toEqual([
      'Source pane',
      'Comparison pane',
    ]);
  });

  it('adds and removes the comparison segment as the property changes', async () => {
    const host = await mount('opm-bottom-bar', { source: footer() });

    host.comparison = footer();
    await host.updateComplete;
    expect(segments(host)).toHaveLength(2);

    host.comparison = null;
    await host.updateComplete;
    expect(segments(host)).toHaveLength(1);
  });

  it('lets each segment share the room and shrink below its content', async () => {
    const host = await mount('opm-bottom-bar', { source: footer(), comparison: footer() });

    for (const element of segments(host)) {
      for (const name of ['flex-1', 'min-w-0', 'flex', 'items-center']) {
        expect(element.classList.contains(name)).toBe(true);
      }
    }
  });

  it('truncates the counts instead of wrapping them', async () => {
    const host = await mount('opm-bottom-bar', { source: footer() });

    const lines = countsOf(host, 'source').querySelectorAll('.truncate');
    expect(lines).toHaveLength(2);
    for (const line of lines) expect(line.classList.contains('text-xs')).toBe(true);
  });
});

describe('opm-bottom-bar counts', () => {
  it('shows the items in the folder and in the subfolders', async () => {
    const host = await mount('opm-bottom-bar', {
      source: footer({ counts: { in_folder: 12, in_subfolders: 340 } }),
    });

    const lines = Array.from(countsOf(host, 'source').querySelectorAll('.truncate')).map((line) =>
      line.textContent?.trim(),
    );
    expect(lines).toEqual([`${formatCount(12)} in folder`, `${formatCount(340)} in subfolders`]);
  });

  it('groups the digits of large counts', async () => {
    const host = await mount('opm-bottom-bar', {
      source: footer({ counts: { in_folder: 1234567, in_subfolders: 9876543 } }),
    });

    const text = countsOf(host, 'source').textContent ?? '';
    expect(text).toContain(`${formatCount(1234567)} in folder`);
    expect(text).toContain(`${formatCount(9876543)} in subfolders`);
    expect(text).not.toContain('1234567');
    expect(text).not.toContain('9876543');
  });

  it('shows zero counts', async () => {
    const host = await mount('opm-bottom-bar', {
      source: footer({ counts: { in_folder: 0, in_subfolders: 0 } }),
    });

    expect(countsOf(host, 'source').textContent).toContain('0 in folder');
    expect(countsOf(host, 'source').textContent).toContain('0 in subfolders');
  });

  it('carries the full text and the filter note in a tooltip', async () => {
    const host = await mount('opm-bottom-bar', {
      source: footer({ counts: { in_folder: 1234567, in_subfolders: 9876543 } }),
    });

    const counts = countsOf(host, 'source');
    expect(counts.classList.contains('tooltip')).toBe(true);
    // The pane at the left edge must not centre its tip over the counts: the shell would clip it.
    expect(counts.classList.contains('tooltip-start')).toBe(true);
    const tip = counts.getAttribute('data-tip') ?? '';
    expect(tip).toContain('current filters');
    expect(tip).toContain(`${formatCount(1234567)} in folder`);
    expect(tip).toContain(`${formatCount(9876543)} in subfolders`);
  });

  it('shows skeletons while the counts are unknown', async () => {
    const host = await mount('opm-bottom-bar', { source: footer({ counts: null }) });

    const counts = countsOf(host, 'source');
    expect(counts.querySelectorAll('.skeleton')).toHaveLength(2);
    expect(counts.querySelector('.truncate')).toBeNull();
    expect(counts.classList.contains('tooltip')).toBe(false);
    expect(counts.hasAttribute('data-tip')).toBe(false);
    expect(counts.querySelector('.sr-only')?.textContent).toBe('Counting items…');
  });

  it('swaps the skeletons for the counts in the same live region', async () => {
    const host = await mount('opm-bottom-bar', { source: footer({ counts: null }) });
    const region = countsOf(host, 'source');

    host.source = footer({ counts: { in_folder: 5, in_subfolders: 6 } });
    await host.updateComplete;

    expect(countsOf(host, 'source')).toBe(region);
    expect(region.querySelector('.skeleton')).toBeNull();
    expect(region.textContent).toContain(`${formatCount(5)} in folder`);
  });

  it('counts each pane on its own', async () => {
    const host = await mount('opm-bottom-bar', {
      source: footer({ counts: { in_folder: 1, in_subfolders: 2 } }),
      comparison: footer({ counts: null }),
    });

    expect(countsOf(host, 'source').textContent).toContain('1 in folder');
    expect(countsOf(host, 'comparison').querySelectorAll('.skeleton')).toHaveLength(2);
  });
});

describe('opm-bottom-bar selection', () => {
  it('shows neither the badge nor the clear button when nothing is selected', async () => {
    const host = await mount('opm-bottom-bar', { source: footer({ selectedCount: 0 }) });

    expect(segment(host, 'source').querySelector('.badge')).toBeNull();
    expect(clearButton(host, 'source')).toBeNull();
    expect(host.textContent).not.toContain('selected');
  });

  it('shows how many items are selected and a clear button', async () => {
    const host = await mount('opm-bottom-bar', { source: footer({ selectedCount: 3 }) });

    const badge = segment(host, 'source').querySelector('.badge.badge-primary');
    expect(badge?.textContent?.trim()).toBe(`${formatCount(3)} selected`);
    const clear = clearButton(host, 'source');
    expect(clear?.classList.contains('btn-ghost')).toBe(true);
    expect(clear?.classList.contains('btn-xs')).toBe(true);
    expect(clear?.classList.contains('btn-circle')).toBe(true);
    expect(clear?.querySelector('opm-icon')?.getAttribute('name')).toBe('x');
  });

  it('drops the word from the badge in a narrow segment, the number stays', async () => {
    const host = await mount('opm-bottom-bar', { source: footer({ selectedCount: 3 }) });

    const word = segment(host, 'source').querySelector('.badge > span');
    expect(word?.textContent?.trim()).toBe('selected');
    expect(word?.classList.contains('hidden')).toBe(true);
    expect(word?.classList.contains('@sm:inline')).toBe(true);
  });

  it('groups the digits of a large selection', async () => {
    const host = await mount('opm-bottom-bar', { source: footer({ selectedCount: 1234567 }) });

    const text = segment(host, 'source').textContent ?? '';
    expect(text).toContain(`${formatCount(1234567)} selected`);
    expect(text).not.toContain('1234567');
  });

  it('announces the selection once, through a status that stays in the page', async () => {
    const host = await mount('opm-bottom-bar', { source: footer({ selectedCount: 0 }) });
    const announcer = segment(host, 'source').querySelector('.sr-only[role="status"]');
    expect(announcer?.textContent).toBe('');

    host.source = footer({ selectedCount: 2 });
    await host.updateComplete;

    expect(segment(host, 'source').querySelector('.sr-only[role="status"]')).toBe(announcer);
    expect(announcer?.textContent).toBe(`${formatCount(2)} selected`);
    expect(segment(host, 'source').querySelector('.badge')?.getAttribute('aria-hidden')).toBe(
      'true',
    );
  });

  it('keeps the selection of each pane apart', async () => {
    const host = await mount('opm-bottom-bar', {
      source: footer({ selectedCount: 4 }),
      comparison: footer({ selectedCount: 0 }),
    });

    expect(segment(host, 'source').querySelector('.badge')).not.toBeNull();
    expect(segment(host, 'comparison').querySelector('.badge')).toBeNull();
    expect(clearButton(host, 'comparison')).toBeNull();
  });

  it('emits clear-selection with the pane of the clear button', async () => {
    const host = await mount('opm-bottom-bar', {
      source: footer({ selectedCount: 1 }),
      comparison: footer({ selectedCount: 2 }),
    });
    const received: unknown[] = [];
    host.addEventListener('clear-selection', (event) => received.push(event.detail));

    clearButton(host, 'comparison')?.click();
    clearButton(host, 'source')?.click();

    expect(received).toEqual([{ pane: 'comparison' }, { pane: 'source' }]);
  });
});

describe('opm-bottom-bar select all', () => {
  it('is an icon button with an accessible name and a label shown when there is room', async () => {
    const host = await mount('opm-bottom-bar', { source: footer() });

    const button = selectAll(host, 'source');
    expect(button.classList.contains('btn-ghost')).toBe(true);
    expect(button.classList.contains('btn-xs')).toBe(true);
    expect(button.querySelector('opm-icon')?.getAttribute('name')).toBe('square-check');
    const label = button.querySelector('span');
    expect(label?.textContent).toBe('Select all');
    expect(label?.classList.contains('hidden')).toBe(true);
    expect(label?.classList.contains('@lg:inline')).toBe(true);
    expect(button.closest('.tooltip')?.getAttribute('data-tip')).toBe('Select all');
  });

  it('is enabled when the folder has items', async () => {
    const host = await mount('opm-bottom-bar', { source: footer() });

    expect(selectAll(host, 'source').disabled).toBe(false);
    expect(selectAll(host, 'source').getAttribute('aria-busy')).toBeNull();
  });

  it('is disabled while the counts are unknown', async () => {
    const host = await mount('opm-bottom-bar', { source: footer({ counts: null }) });

    expect(selectAll(host, 'source').disabled).toBe(true);
  });

  it('is disabled when the folder has no item, even with items in the subfolders', async () => {
    const host = await mount('opm-bottom-bar', {
      source: footer({ counts: { in_folder: 0, in_subfolders: 9 } }),
    });

    expect(selectAll(host, 'source').disabled).toBe(true);
  });

  it('shows a spinner and is disabled while selecting', async () => {
    const host = await mount('opm-bottom-bar', { source: footer({ selecting: true }) });

    const button = selectAll(host, 'source');
    expect(button.disabled).toBe(true);
    expect(button.getAttribute('aria-busy')).toBe('true');
    const spinner = button.querySelector('.loading.loading-spinner.loading-xs');
    expect(spinner).not.toBeNull();
    expect(button.querySelector('opm-icon')).toBeNull();
  });

  it('goes back to the icon when the selection is done', async () => {
    const host = await mount('opm-bottom-bar', { source: footer({ selecting: true }) });

    host.source = footer({ selecting: false, selectedCount: 12 });
    await host.updateComplete;

    expect(selectAll(host, 'source').querySelector('.loading')).toBeNull();
    expect(selectAll(host, 'source').querySelector('opm-icon')).not.toBeNull();
  });

  it('only disables the pane that is selecting', async () => {
    const host = await mount('opm-bottom-bar', {
      source: footer({ selecting: true }),
      comparison: footer(),
    });

    expect(selectAll(host, 'source').disabled).toBe(true);
    expect(selectAll(host, 'comparison').disabled).toBe(false);
  });

  it('emits select-all with the pane of the button, and only when enabled', async () => {
    const host = await mount('opm-bottom-bar', {
      source: footer(),
      comparison: footer({ counts: { in_folder: 0, in_subfolders: 0 } }),
    });
    const received: unknown[] = [];
    const onBody = vi.fn();
    host.addEventListener('select-all', (event) => received.push(event.detail));
    document.body.addEventListener('select-all', onBody);

    selectAll(host, 'comparison').click();
    expect(received).toEqual([]);

    selectAll(host, 'source').click();
    expect(received).toEqual([{ pane: 'source' }]);
    expect(onBody).toHaveBeenCalledOnce();
    document.body.removeEventListener('select-all', onBody);
  });
});

describe('opm-bottom-bar slider', () => {
  it('is a small DaisyUI range with the bounds of its pane', async () => {
    const host = await mount('opm-bottom-bar', {
      source: footer({ size: 128, sizeMin: 64, sizeMax: 256, sizeStep: 32 }),
      comparison: footer({ size: 200, sizeMin: 100, sizeMax: 400, sizeStep: 50 }),
    });

    const source = slider(host, 'source');
    expect(source.classList.contains('range')).toBe(true);
    expect(source.classList.contains('range-xs')).toBe(true);
    expect(source.classList.contains('w-24')).toBe(true);
    expect(source.getAttribute('aria-label')).toBe('Thumbnail size');
    expect([source.min, source.max, source.step, source.value]).toEqual(['64', '256', '32', '128']);
    expect(segment(host, 'comparison').querySelector('input[type="range"]')).toBeNull();
  });

  it('shrinks with its segment and is hidden when the segment is narrower than 14rem', async () => {
    const host = await mount('opm-bottom-bar', { source: footer(), comparison: footer() });

    for (const pane of ['source'] as const) {
      const { classList } = slider(host, pane);
      expect(classList.contains('shrink')).toBe(true);
      expect(classList.contains('min-w-12')).toBe(true);
      expect(classList.contains('hidden')).toBe(true);
      expect(classList.contains('@[14rem]:block')).toBe(true);
    }
  });

  it('reports every input event with the pane and the size, so the grid resizes live', async () => {
    const host = await mount('opm-bottom-bar', { source: footer(), comparison: footer() });
    const received: unknown[] = [];
    const onBody = vi.fn();
    host.addEventListener('size-change', (event) => received.push(event.detail));
    document.body.addEventListener('size-change', onBody);

    drag(slider(host, 'source'), 192);
    drag(slider(host, 'source'), 208);

    expect(received).toEqual([
      { pane: 'source', size: 192 },
      { pane: 'source', size: 208 },
    ]);
    expect(onBody).toHaveBeenCalledTimes(2);
    document.body.removeEventListener('size-change', onBody);
  });

  it('reports a number, not a string', async () => {
    const host = await mount('opm-bottom-bar', { source: footer() });
    let size: unknown;
    host.addEventListener('size-change', (event) => {
      size = event.detail.size;
    });

    drag(slider(host, 'source'), 224);

    expect(size).toBe(224);
  });

  it('follows the size property', async () => {
    const host = await mount('opm-bottom-bar', { source: footer({ size: 160 }) });

    host.source = footer({ size: 240 });
    await host.updateComplete;

    expect(slider(host, 'source').value).toBe('240');
  });

  it('shows the size of the property when the parent did not accept the change', async () => {
    const host = await mount('opm-bottom-bar', { source: footer({ size: 160 }) });

    drag(slider(host, 'source'), 288);
    host.source = footer({ size: 160 });
    await host.updateComplete;

    expect(slider(host, 'source').value).toBe('160');
  });

  it('applies new bounds together with a size that only fits them', async () => {
    const host = await mount('opm-bottom-bar', { source: footer({ size: 160, sizeMax: 320 }) });

    host.source = footer({ size: 500, sizeMin: 96, sizeMax: 640 });
    await host.updateComplete;

    expect(slider(host, 'source').max).toBe('640');
    expect(slider(host, 'source').value).toBe('500');
  });
});

describe('opm-bottom-bar active pane', () => {
  it('marks only the active segment with a primary top border', async () => {
    const host = await mount('opm-bottom-bar', {
      source: footer({ active: false }),
      comparison: footer({ active: true }),
    });

    const source = segment(host, 'source');
    const comparison = segment(host, 'comparison');
    expect(comparison.classList.contains('border-t-primary')).toBe(true);
    expect(comparison.classList.contains('border-t-transparent')).toBe(false);
    expect(source.classList.contains('border-t-transparent')).toBe(true);
    expect(source.classList.contains('border-t-primary')).toBe(false);
  });

  it('keeps the border width of every segment, so nothing shifts', async () => {
    const host = await mount('opm-bottom-bar', {
      source: footer({ active: true }),
      comparison: footer({ active: false }),
    });

    for (const element of segments(host)) {
      expect(element.classList.contains('border-t-2')).toBe(true);
    }
  });

  it('moves the highlight when the active pane changes', async () => {
    const host = await mount('opm-bottom-bar', {
      source: footer({ active: true }),
      comparison: footer({ active: false }),
    });

    host.source = footer({ active: false });
    host.comparison = footer({ active: true });
    await host.updateComplete;

    expect(segment(host, 'source').classList.contains('border-t-primary')).toBe(false);
    expect(segment(host, 'comparison').classList.contains('border-t-primary')).toBe(true);
  });

  it('keeps the highlight apart from the divider between the segments', async () => {
    const host = await mount('opm-bottom-bar', {
      source: footer({ active: false }),
      comparison: footer({ active: true }),
    });

    const source = segment(host, 'source');
    const comparison = segment(host, 'comparison');
    expect(source.classList.contains('border-s')).toBe(false);
    expect(comparison.classList.contains('border-s')).toBe(true);
    expect(comparison.classList.contains('border-s-base-300')).toBe(true);
    expect(comparison.classList.contains('border-primary')).toBe(false);
  });
});

describe('opm-bottom-bar actions', () => {
  it('renders no actions area without actions', async () => {
    const host = await mount('opm-bottom-bar', { source: footer() });

    expect(host.querySelector('.flex-none')).toBeNull();
  });

  it('keeps actions inside the comparison half without shifting the pane divider', async () => {
    const host = await mount('opm-bottom-bar', {
      source: footer(),
      comparison: footer(),
      actions: html`<button type="button" id="go">Go</button>`,
    });

    const area = host.querySelector('.flex-none');
    expect(area?.classList.contains('items-center')).toBe(true);
    expect(area?.querySelector('#go')?.textContent).toBe('Go');
    const bar = host.querySelector('.navbar');
    expect(bar?.lastElementChild).toBe(segment(host, 'comparison'));
    expect(segment(host, 'comparison').lastElementChild).toBe(area);
    expect(bar?.classList.contains('md:grid-cols-2')).toBe(true);
  });

  it('updates and removes the actions with the property', async () => {
    const host = await mount('opm-bottom-bar', {
      source: footer(),
      actions: html`<span id="first">one</span>`,
    });

    host.actions = html`<span id="second">two</span>`;
    await host.updateComplete;
    expect(host.querySelector('#first')).toBeNull();
    expect(host.querySelector('#second')).not.toBeNull();

    host.actions = undefined;
    await host.updateComplete;
    expect(host.querySelector('#second')).toBeNull();
    expect(host.querySelector('.flex-none')).toBeNull();
  });

  it('hosts a split button whose events reach the page through the bar', async () => {
    const host = await mount('opm-bottom-bar', {
      source: footer(),
      actions: html`<opm-split-button
        .options=${[{ id: 'move', label: 'Move files' }]}
        .count=${12}
      ></opm-split-button>`,
    });
    const split = host.querySelector<OpmSplitButton>('opm-split-button');
    await split?.updateComplete;
    const received: string[] = [];
    host.addEventListener('split-action', (event) => received.push(event.detail.optionId));

    split?.querySelector<HTMLButtonElement>('.join > button')?.click();

    expect(received).toEqual(['move']);
  });
});
