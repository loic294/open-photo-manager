import { describe, expect, it } from 'vitest';
import { ApmIcon, ICON_NAMES, type IconName, isIconName } from '@/components/apm-icon';
import { mount } from '@/test/helpers';

describe('apm-icon', () => {
  it('renders the svg of the named icon, decorative by default', async () => {
    const icon = await mount('apm-icon', { name: 'trash-2' });

    const svg = icon.querySelector('svg');
    expect(svg).not.toBeNull();
    expect(svg?.getAttribute('aria-hidden')).toBe('true');
    expect(svg?.hasAttribute('role')).toBe(false);
    expect(svg?.getAttribute('stroke')).toBe('currentColor');
    expect(svg?.getAttribute('viewBox')).toBe('0 0 24 24');
    expect(svg?.querySelectorAll('path').length).toBeGreaterThan(0);
  });

  it('is an image with an accessible name when given a label', async () => {
    const icon = await mount('apm-icon', { name: 'check', label: 'Saved' });

    const svg = icon.querySelector('svg');
    expect(svg?.getAttribute('role')).toBe('img');
    expect(svg?.getAttribute('aria-label')).toBe('Saved');
    expect(svg?.hasAttribute('aria-hidden')).toBe(false);
  });

  it.each([
    ['xs', 'size-3'],
    ['sm', 'size-4'],
    ['md', 'size-5'],
    ['lg', 'size-6'],
    ['xl', 'size-8'],
  ] as const)('maps size %s to %s', async (size, expected) => {
    const icon = await mount('apm-icon', { name: 'info', size });

    expect(icon.querySelector('svg')?.classList.contains(expected)).toBe(true);
  });

  it('defaults to the medium size', async () => {
    const icon = await mount('apm-icon', { name: 'info' });

    expect(icon.size).toBe('md');
    expect(icon.querySelector('svg')?.classList.contains('size-5')).toBe(true);
  });

  it('updates when the name changes', async () => {
    const icon = await mount('apm-icon', { name: 'sun' });
    const before = icon.querySelector('svg')?.innerHTML;

    icon.name = 'moon';
    await icon.updateComplete;

    expect(icon.querySelector('svg')?.innerHTML).not.toBe(before);
  });

  it('renders nothing for an unknown name', async () => {
    const icon = await mount('apm-icon', { name: 'does-not-exist' as IconName });

    expect(icon.querySelector('svg')).toBeNull();
  });

  it('lays out as an inline flex box so it centres inside buttons and text', async () => {
    const icon = await mount('apm-icon', { name: 'x' });

    expect(icon.classList.contains('inline-flex')).toBe(true);
    expect(icon.classList.contains('shrink-0')).toBe(true);
    expect(icon.querySelector('svg')?.classList.contains('block')).toBe(true);
  });

  it('draws every registered icon', async () => {
    for (const name of ICON_NAMES) {
      const icon = await mount(new ApmIcon(), { name });

      expect(icon.querySelector('svg')?.children.length, name).toBeGreaterThan(0);
    }
  });

  it('only accepts own keys of the registry', () => {
    expect(isIconName('trash-2')).toBe(true);
    expect(isIconName('toString')).toBe(false);
    expect(isIconName(3)).toBe(false);
    expect(ICON_NAMES).toContain('images');
  });
});
