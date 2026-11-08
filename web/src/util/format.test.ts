import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  EM_DASH,
  formatCaptureDate,
  formatDuration,
  formatEta,
  formatFileSize,
  formatFileTime,
  formatRate,
} from '@/util/format';

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

describe('formatFileTime', () => {
  // 2023-11-14T22:13:20Z
  const ts = 1_700_000_000;

  const originalZone = process.env.TZ;
  afterEach(() => {
    if (originalZone === undefined) delete process.env.TZ;
    else process.env.TZ = originalZone;
    vi.resetModules();
  });

  const plain = (text: string) => text.replace(/\s/g, ' ');

  it('reads the instant on the browser clock, whatever the zone', async () => {
    const expected = {
      'Asia/Kolkata': 'Nov 15, 2023, 3:43 AM',
      'America/Los_Angeles': 'Nov 14, 2023, 2:13 PM',
      UTC: 'Nov 14, 2023, 10:13 PM',
    };
    for (const [zone, text] of Object.entries(expected)) {
      process.env.TZ = zone;
      // The formatter is cached per module: load a fresh one so it is built under this zone.
      vi.resetModules();
      const fresh = await import('@/util/format');
      expect(plain(fresh.formatFileTime(ts, 'en-US'))).toBe(text);
    }
  });

  it('differs from the capture date, which ignores the zone', async () => {
    process.env.TZ = 'Asia/Kolkata';
    vi.resetModules();
    const fresh = await import('@/util/format');
    expect(plain(fresh.formatCaptureDate(ts, 'en-US'))).toBe('Nov 14, 2023, 10:13 PM');
    expect(plain(fresh.formatFileTime(ts, 'en-US'))).not.toBe(
      plain(fresh.formatCaptureDate(ts, 'en-US')),
    );
  });

  it('follows the locale', () => {
    expect(formatFileTime(ts, 'fr-FR')).toContain('2023');
  });

  it('shows an em dash without a usable time', () => {
    expect(formatFileTime(null)).toBe(EM_DASH);
    expect(formatFileTime(undefined)).toBe(EM_DASH);
    expect(formatFileTime(Number.NaN)).toBe(EM_DASH);
    expect(formatFileTime(Number.POSITIVE_INFINITY)).toBe(EM_DASH);
    expect(formatFileTime(1e20)).toBe(EM_DASH);
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

describe('formatRate', () => {
  const MB = 1024 * 1024;

  it.each([
    [0, '0 B/s'],
    [0.4, '0 B/s'],
    [512, '512 B/s'],
    [1024, '1 KB/s'],
    [1.5 * MB, '1.5 MB/s'],
    [42 * MB, '42 MB/s'],
    [MB - 1, '1 MB/s'],
    [3 * 1024 ** 3, '3 GB/s'],
    [5000 * 1024 ** 4, '5000 TB/s'],
  ])('formats %d bytes per second as %s', (perSecond, text) => {
    expect(formatRate(perSecond, 'bytes', 'en-US')).toBe(text);
  });

  it.each([
    [0, '0 items/s'],
    [0.04, '0 items/s'],
    [0.4, '0.4 items/s'],
    [0.96, '1 item/s'],
    [1, '1 item/s'],
    [1.04, '1 item/s'],
    [1.5, '1.5 items/s'],
    [9.94, '9.9 items/s'],
    [9.96, '10 items/s'],
    [10, '10 items/s'],
    [85, '85 items/s'],
    [85.4, '85 items/s'],
    [999.6, '1,000 items/s'],
    [2400, '2,400 items/s'],
    [1_234_567, '1,234,567 items/s'],
  ])('formats %d items per second as %s', (perSecond, text) => {
    expect(formatRate(perSecond, 'items', 'en-US')).toBe(text);
  });

  it('follows the locale for the decimal separator', () => {
    expect(formatRate(1536, 'bytes', 'fr-FR')).toMatch(/^1,5\s?KB\/s$/);
    expect(formatRate(2.5, 'items', 'fr-FR')).toBe('2,5 items/s');
  });

  it('shows an em dash for a rate that is not usable', () => {
    for (const unit of ['bytes', 'items'] as const) {
      expect(formatRate(null, unit)).toBe(EM_DASH);
      expect(formatRate(undefined, unit)).toBe(EM_DASH);
      expect(formatRate(-1, unit)).toBe(EM_DASH);
      expect(formatRate(Number.NaN, unit)).toBe(EM_DASH);
      expect(formatRate(Number.POSITIVE_INFINITY, unit)).toBe(EM_DASH);
    }
  });
});

describe('formatEta', () => {
  const MINUTE = 60;
  const HOUR = 3600;

  it.each([
    [0, 'under 1 min'],
    [0.4, 'under 1 min'],
    [59, 'under 1 min'],
    [59.9, 'under 1 min'],
    [MINUTE, 'about 1 min'],
    [89, 'about 1 min'],
    [90, 'about 2 min'],
    [3 * MINUTE, 'about 3 min'],
    [3 * MINUTE + 29, 'about 3 min'],
    [3 * MINUTE + 30, 'about 4 min'],
    [59 * MINUTE + 29, 'about 59 min'],
    [59 * MINUTE + 30, 'about 1 h'],
    [HOUR, 'about 1 h'],
    [HOUR + 5 * MINUTE, 'about 1 h 5 min'],
    [2 * HOUR + 10 * MINUTE, 'about 2 h 10 min'],
    [23 * HOUR + 59 * MINUTE, 'about 23 h 59 min'],
    [23 * HOUR + 59 * MINUTE + 29, 'about 23 h 59 min'],
    [23 * HOUR + 59 * MINUTE + 30, 'more than a day'],
    [24 * HOUR, 'more than a day'],
    [30 * 24 * HOUR, 'more than a day'],
    [1e15, 'more than a day'],
  ])('formats %d seconds as "%s"', (seconds, text) => {
    expect(formatEta(seconds)).toBe(text);
  });

  it('never shows a negative time', () => {
    expect(formatEta(-1)).toBe('under 1 min');
    expect(formatEta(-3 * HOUR)).toBe('under 1 min');
    expect(formatEta(Number.NEGATIVE_INFINITY)).toBe('under 1 min');
  });

  it('reads an infinite estimate as more than a day', () => {
    expect(formatEta(Number.POSITIVE_INFINITY)).toBe('more than a day');
  });

  it('shows an em dash when there is no estimate', () => {
    expect(formatEta(null)).toBe(EM_DASH);
    expect(formatEta(undefined)).toBe(EM_DASH);
    expect(formatEta(Number.NaN)).toBe(EM_DASH);
  });
});
