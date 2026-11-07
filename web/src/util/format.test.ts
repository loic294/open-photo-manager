import { afterEach, describe, expect, it, vi } from 'vitest';
import { EM_DASH, formatCaptureDate, formatDuration, formatFileSize } from '@/util/format';

describe('formatCaptureDate', () => {
  // 2023-11-14T22:13:20Z
  const ts = 1_700_000_000;

  const originalZone = process.env.TZ;
  afterEach(() => {
    if (originalZone === undefined) delete process.env.TZ;
    else process.env.TZ = originalZone;
  });

  // ICU versions differ in the space before AM/PM (plain or narrow no-break).
  const plain = (text: string) => text.replace(/\s/g, ' ');

  it('reads the naive-local seconds as a wall clock', () => {
    expect(plain(formatCaptureDate(ts, 'en-US'))).toBe('Nov 14, 2023, 10:13 PM');
    expect(plain(formatCaptureDate(0, 'en-US'))).toBe('Jan 1, 1970, 12:00 AM');
  });

  it('does not depend on the browser time zone', async () => {
    const expected = plain(formatCaptureDate(ts, 'en-US'));
    for (const zone of ['Pacific/Kiritimati', 'America/Los_Angeles', 'Asia/Kolkata']) {
      process.env.TZ = zone;
      // The formatter is cached per module: load a fresh one so it is built under this zone.
      vi.resetModules();
      const fresh = await import('@/util/format');
      // The zone really changed the local clock (otherwise this test proves nothing).
      expect(new Date(ts * 1000).getHours()).not.toBe(new Date(ts * 1000).getUTCHours());
      expect(plain(fresh.formatCaptureDate(ts, 'en-US'))).toBe(expected);
    }
  });

  it('follows the locale', () => {
    expect(formatCaptureDate(ts, 'fr-FR')).toContain('22:13');
    expect(formatCaptureDate(ts, 'fr-FR')).toContain('2023');
  });

  it('uses the default locale when none is given', () => {
    expect(formatCaptureDate(ts)).toContain('2023');
  });

  it('shows an em dash without a usable date', () => {
    expect(formatCaptureDate(null)).toBe(EM_DASH);
    expect(formatCaptureDate(undefined)).toBe(EM_DASH);
    expect(formatCaptureDate(Number.NaN)).toBe(EM_DASH);
    expect(formatCaptureDate(Number.POSITIVE_INFINITY)).toBe(EM_DASH);
    expect(formatCaptureDate(1e20)).toBe(EM_DASH);
  });
});

describe('formatFileSize', () => {
  it.each([
    [0, '0 B'],
    [1, '1 B'],
    [1023, '1023 B'],
    [1024, '1 KB'],
    [1536, '1.5 KB'],
    [10 * 1024, '10 KB'],
    [250 * 1024, '250 KB'],
    [1024 * 1024 - 1, '1 MB'],
    [1_572_864, '1.5 MB'],
    [15 * 1024 * 1024, '15 MB'],
    [5 * 1024 ** 3, '5 GB'],
    [2.5 * 1024 ** 4, '2.5 TB'],
    [5000 * 1024 ** 4, '5000 TB'],
  ])('formats %d bytes as %s', (bytes, text) => {
    expect(formatFileSize(bytes, 'en-US')).toBe(text);
  });

  it('follows the locale for the decimal separator', () => {
    expect(formatFileSize(1536, 'fr-FR')).toMatch(/^1,5\s?KB$/);
  });

  it('shows an em dash for a value that is not a size', () => {
    expect(formatFileSize(-1)).toBe(EM_DASH);
    expect(formatFileSize(Number.NaN)).toBe(EM_DASH);
    expect(formatFileSize(Number.POSITIVE_INFINITY)).toBe(EM_DASH);
  });
});

describe('formatDuration', () => {
  it.each([
    [0, '0:00'],
    [400, '0:00'],
    [500, '0:01'],
    [1000, '0:01'],
    [9_000, '0:09'],
    [61_000, '1:01'],
    [59_600, '1:00'],
    [3_599_400, '59:59'],
    [3_599_500, '1:00:00'],
    [3_661_000, '1:01:01'],
    [36_000_000, '10:00:00'],
    [90_000_000, '25:00:00'],
  ])('formats %d ms as %s', (ms, text) => {
    expect(formatDuration(ms)).toBe(text);
  });

  it('reads a negative or non-finite value as zero', () => {
    expect(formatDuration(-5000)).toBe('0:00');
    expect(formatDuration(Number.NaN)).toBe('0:00');
    expect(formatDuration(Number.POSITIVE_INFINITY)).toBe('0:00');
  });

  it('shows an em dash for a missing value', () => {
    expect(formatDuration(null)).toBe(EM_DASH);
    expect(formatDuration(undefined)).toBe(EM_DASH);
  });
});
