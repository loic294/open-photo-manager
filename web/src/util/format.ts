/** Shown where a value is missing. */
export const EM_DASH = '\u2014';

const dateFormats = new Map<string, Intl.DateTimeFormat>();
const numberFormats = new Map<string, Intl.NumberFormat>();

function dateFormat(locale: string | undefined): Intl.DateTimeFormat {
  const key = locale ?? '';
  let format = dateFormats.get(key);
  if (!format) {
    // `capture_ts` is naive-local: its UTC reading IS the photographer's wall clock.
    format = new Intl.DateTimeFormat(locale, {
      dateStyle: 'medium',
      timeStyle: 'short',
      timeZone: 'UTC',
    });
    dateFormats.set(key, format);
  }
  return format;
}

function numberFormat(locale: string | undefined, fractionDigits: number): Intl.NumberFormat {
  const key = `${locale ?? ''}|${fractionDigits}`;
  let format = numberFormats.get(key);
  if (!format) {
    format = new Intl.NumberFormat(locale, {
      maximumFractionDigits: fractionDigits,
      useGrouping: false,
    });
    numberFormats.set(key, format);
  }
  return format;
}

/**
 * A capture time (`FileItem.capture_ts`: epoch seconds of the photographer's wall clock, not of
 * the instant) as "Nov 14, 2023, 10:13 PM". It is read in UTC whatever the browser's time zone, so
 * the time shown is the one the camera showed. An em dash when there is no date.
 */
export function formatCaptureDate(ts: number | null | undefined, locale?: string): string {
  if (typeof ts !== 'number' || !Number.isFinite(ts)) return EM_DASH;
  const date = new Date(ts * 1000);
  return Number.isNaN(date.getTime()) ? EM_DASH : dateFormat(locale).format(date);
}

const SIZE_UNITS = ['B', 'KB', 'MB', 'GB', 'TB'] as const;

/** One decimal below 10 ("1.5 MB"), none above ("15 MB", "980 KB"). */
function fractionDigits(value: number, unit: number): number {
  return unit === 0 || value >= 10 ? 0 : 1;
}

/** A byte count in binary units (1 KB = 1024 B): "512 B", "1.5 MB", "12 GB". */
export function formatFileSize(bytes: number, locale?: string): string {
  if (!Number.isFinite(bytes) || bytes < 0) return EM_DASH;
  let value = bytes;
  let unit = 0;
  // Step up while the rounded figure would read 1024 ("1024 KB" is "1 MB").
  while (unit < SIZE_UNITS.length - 1) {
    const shown = Number(value.toFixed(fractionDigits(value, unit)));
    if (shown < 1024) break;
    value /= 1024;
    unit += 1;
  }
  return `${numberFormat(locale, fractionDigits(value, unit)).format(value)} ${SIZE_UNITS[unit]}`;
}

/**
 * A media duration in milliseconds as "m:ss", or "h:mm:ss" from one hour, rounded to the nearest
 * second. A missing value is an em dash; a negative or non-finite one reads "0:00".
 */
export function formatDuration(ms: number | null | undefined): string {
  if (ms === null || ms === undefined) return EM_DASH;
  const total = Number.isFinite(ms) && ms > 0 ? Math.round(ms / 1000) : 0;
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = String(total % 60).padStart(2, '0');
  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, '0')}:${seconds}`
    : `${minutes}:${seconds}`;
}
