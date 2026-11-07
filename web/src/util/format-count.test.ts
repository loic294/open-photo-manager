import { describe, expect, it } from 'vitest';
import { formatCount } from '@/util/format-count';

// The separators depend on the locale of the machine running the tests.
const reference = new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 });

describe('formatCount', () => {
  it('writes a small number as it is', () => {
    expect(formatCount(0)).toBe(reference.format(0));
    expect(formatCount(7)).toBe(reference.format(7));
    expect(formatCount(999)).toBe(reference.format(999));
  });

  it('groups the digits of a large number like the locale does', () => {
    const text = formatCount(1_234_567);

    expect(text).toBe(reference.format(1_234_567));
    expect(text.length).toBeGreaterThan('1234567'.length);
    expect(text.replace(/\D/g, '')).toBe('1234567');
  });

  it('keeps 100 000 items readable', () => {
    expect(formatCount(100_000).replace(/\D/g, '')).toBe('100000');
    expect(formatCount(100_000)).not.toBe('100000');
  });

  it('rounds to a whole number', () => {
    expect(formatCount(2.6)).toBe(reference.format(3));
  });
});
