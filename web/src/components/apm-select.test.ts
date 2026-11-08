import { afterEach, describe, expect, it, vi } from 'vitest';
import '@/components/apm-select';
import type { ApmSelect, SelectChangeEvent, SelectOption } from '@/components/apm-select';
import { mount } from '@/test/helpers';

const OPTIONS: SelectOption[] = [
  { value: 'name', label: 'Name' },
  { value: 'date', label: 'Date', title: 'Capture date' },
  { value: 'size', label: 'Size', disabled: true },
];

it('allows a long labelled control to shrink within its available width', async () => {
  const el = await mount('apm-select', { options: OPTIONS, label: 'Match by', showLabel: true });
  expect(el.querySelector('label')?.classList.contains('max-w-full')).toBe(true);
  expect(el.querySelector('label')?.classList.contains('min-w-0')).toBe(true);
  expect(el.querySelector('select')?.classList.contains('min-w-0')).toBe(true);
});

afterEach(() => {
  vi.restoreAllMocks();
});

function nativeSelect(host: ApmSelect): HTMLSelectElement {
  const select = host.querySelector('select');
  if (!select) throw new Error('no select rendered');
  return select;
}

function choose(host: ApmSelect, value: string): void {
  const select = nativeSelect(host);
  select.value = value;
  select.dispatchEvent(new Event('change', { bubbles: true }));
}

async function mountSelect(props: Partial<ApmSelect> = {}): Promise<ApmSelect> {
  return mount('apm-select', { options: OPTIONS, value: 'name', label: 'Sort by', ...props });
}

describe('apm-select', () => {
  it('renders a plain DaisyUI select named by aria-label', async () => {
    const host = await mountSelect();

    const select = nativeSelect(host);
    expect(select.classList.contains('select')).toBe(true);
    expect(select.getAttribute('aria-label')).toBe('Sort by');
    expect(select.title).toBe('Sort by');
    expect(host.querySelector('label')).toBeNull();
  });

  it('lists the options in order, with their tooltip and disabled state', async () => {
    const host = await mountSelect();

    const options = Array.from(host.querySelectorAll('option'));
    expect(options.map((option) => option.value)).toEqual(['name', 'date', 'size']);
    expect(options.map((option) => option.textContent)).toEqual(['Name', 'Date', 'Size']);
    expect(options.map((option) => option.title)).toEqual(['', 'Capture date', '']);
    expect(options.map((option) => option.disabled)).toEqual([false, false, true]);
  });

  it('selects the option of `value`', async () => {
    const host = await mountSelect({ value: 'date' });

    expect(nativeSelect(host).value).toBe('date');
  });

  it('follows `value` and `options` when they change', async () => {
    const host = await mountSelect({ value: 'date' });

    host.value = 'name';
    await host.updateComplete;
    expect(nativeSelect(host).value).toBe('name');

    host.options = [
      { value: 'a', label: 'A' },
      { value: 'b', label: 'B' },
    ];
    host.value = 'b';
    await host.updateComplete;
    expect(Array.from(host.querySelectorAll('option')).map((option) => option.value)).toEqual([
      'a',
      'b',
    ]);
    expect(nativeSelect(host).value).toBe('b');
  });

  it('shows an empty control for a value no option has', async () => {
    const host = await mountSelect({ value: 'nothing' });

    expect(nativeSelect(host).selectedIndex).toBe(-1);
  });

  describe('label', () => {
    it('is hidden by default: the name is only for assistive technology', async () => {
      const host = await mountSelect();

      expect(host.querySelector('.label')).toBeNull();
    });

    it('is shown inside the control with show-label, and still names the select', async () => {
      const host = await mountSelect({ showLabel: true });

      const wrapper = host.querySelector('label');
      expect(wrapper?.classList.contains('select')).toBe(true);
      expect(wrapper?.querySelector('span.label')?.textContent).toBe('Sort by');
      const select = wrapper?.querySelector('select');
      expect(select?.getAttribute('aria-label')).toBe('Sort by');
      expect(select?.classList.contains('select')).toBe(false);
      expect(select?.hasAttribute('title')).toBe(false);
    });

    it('can be switched on and off', async () => {
      const host = await mountSelect();

      host.showLabel = true;
      await host.updateComplete;
      expect(host.querySelector('label span.label')).not.toBeNull();

      host.showLabel = false;
      await host.updateComplete;
      expect(host.querySelector('label')).toBeNull();
      expect(nativeSelect(host).value).toBe('name');
    });

    it('reads the show-label attribute', async () => {
      const host = document.createElement('apm-select');
      host.setAttribute('show-label', '');
      host.label = 'Order';

      await mount(host, { options: OPTIONS, value: 'date' });

      expect(host.querySelector('span.label')?.textContent).toBe('Order');
      expect(nativeSelect(host).value).toBe('date');
    });

    it('has no tooltip without a label', async () => {
      const host = await mountSelect({ label: '' });

      expect(nativeSelect(host).hasAttribute('title')).toBe(false);
    });
  });

  describe('size', () => {
    it.each([
      ['xs', 'select-xs'],
      ['sm', 'select-sm'],
      ['md', 'select-md'],
    ] as const)('%s uses %s', async (size, expected) => {
      const plain = await mountSelect({ size });
      const labelled = await mountSelect({ size, showLabel: true });

      expect(nativeSelect(plain).classList.contains(expected)).toBe(true);
      expect(labelled.querySelector('label')?.classList.contains(expected)).toBe(true);
    });

    it('is small by default', async () => {
      const host = await mountSelect();

      expect(nativeSelect(host).classList.contains('select-sm')).toBe(true);
    });

    it('falls back to the default for a size it does not know', async () => {
      const host = await mountSelect({ size: 'huge' as ApmSelect['size'] });

      expect(nativeSelect(host).classList.contains('select-sm')).toBe(true);
    });

    it('is as wide as its entries instead of the DaisyUI default width', async () => {
      const host = await mountSelect();

      expect(nativeSelect(host).classList.contains('w-auto')).toBe(true);
      expect(host.classList.contains('inline-flex')).toBe(true);
    });
  });

  describe('disabled', () => {
    it('disables the select', async () => {
      const host = await mountSelect({ disabled: true });

      expect(nativeSelect(host).disabled).toBe(true);
    });

    it('is enabled by default', async () => {
      const host = await mountSelect();

      expect(nativeSelect(host).disabled).toBe(false);
    });
  });

  describe('select-change', () => {
    it('reports the chosen value and bubbles', async () => {
      const host = await mountSelect();
      const seen = vi.fn();
      document.body.addEventListener('select-change', seen);

      choose(host, 'date');

      document.body.removeEventListener('select-change', seen);
      expect(seen).toHaveBeenCalledTimes(1);
      const event = seen.mock.calls[0]?.[0] as SelectChangeEvent;
      expect(event.detail).toEqual({ value: 'date' });
      expect(event.target).toBe(host);
    });

    it('keeps the native change event to itself', async () => {
      const host = await mountSelect();
      const leaked = vi.fn();
      document.body.addEventListener('change', leaked);

      choose(host, 'date');

      document.body.removeEventListener('change', leaked);
      expect(leaked).not.toHaveBeenCalled();
    });

    it('does not fire for a change of the properties', async () => {
      const host = await mountSelect();
      const seen = vi.fn();
      host.addEventListener('select-change', seen);

      host.value = 'date';
      await host.updateComplete;

      expect(seen).not.toHaveBeenCalled();
    });

    it('shows the new value once the host accepts it', async () => {
      const host = await mountSelect();
      host.addEventListener('select-change', (event) => {
        host.value = (event as SelectChangeEvent).detail.value;
      });

      choose(host, 'date');
      await host.updateComplete;

      expect(host.value).toBe('date');
      expect(nativeSelect(host).value).toBe('date');
    });

    it('snaps back to its value when the host does not accept the change', async () => {
      const host = await mountSelect({ value: 'name' });
      const seen = vi.fn();
      host.addEventListener('select-change', seen);

      choose(host, 'date');
      expect(nativeSelect(host).value).toBe('date');
      await host.updateComplete;

      expect(seen).toHaveBeenCalledTimes(1);
      expect(host.value).toBe('name');
      expect(nativeSelect(host).value).toBe('name');
    });

    it('snaps back every time the user tries again', async () => {
      const host = await mountSelect({ value: 'name' });

      for (const value of ['date', 'name', 'date']) {
        choose(host, value);
        await host.updateComplete;
        expect(nativeSelect(host).value).toBe('name');
      }
    });

    it('snaps back to a value the host replaces it with', async () => {
      const host = await mountSelect({ value: 'name' });
      host.addEventListener('select-change', () => {
        host.value = 'size';
      });

      choose(host, 'date');
      await host.updateComplete;

      expect(nativeSelect(host).value).toBe('size');
    });
  });
});
