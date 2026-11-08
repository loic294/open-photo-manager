/** Shown where a value is missing. */
export const EM_DASH = '\u2014';

const dateFormats = new Map<string, Intl.DateTimeFormat>();
const numberFormats = new Map<string, Intl.NumberFormat>();

/** `utc` reads the clock of the epoch seconds; otherwise the browser's own time zone applies. */
function dateFormat(locale: string | undefined, utc: boolean): Intl.DateTimeFormat {
  const key = `${locale ?? ''}|${utc ? 'utc' : 'local'}`;
  let format = dateFormats.get(key);
  if (!format) {
    format = new Intl.DateTimeFormat(locale, {
      dateStyle: 'medium',
      timeStyle: 'short',
      ...(utc ? { timeZone: 'UTC' } : {}),
    });
    dateFormats.set(key, format);
  }
  return format;
}

function formatSeconds(
  ts: number | null | undefined,
  locale: string | undefined,
  utc: boolean,
): string {
  if (typeof ts !== 'number' || !Number.isFinite(ts)) return EM_DASH;
  const date = new Date(ts * 1000);
  return Number.isNaN(date.getTime()) ? EM_DASH : dateFormat(locale, utc).format(date);
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
  return formatSeconds(ts, locale, true);
}

/**
 * A real instant (`FileItem.mtime`, `FileDetail.btime`: epoch seconds) as "Nov 14, 2023, 10:13 PM"
 * on the browser's own clock, unlike `formatCaptureDate`. An em dash when there is no time.
 */
export function formatFileTime(ts: number | null | undefined, locale?: string): string {
  return formatSeconds(ts, locale, false);
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

/** What a task's `speed` counts: bytes per second when it declared a byte total, else items. */
export type RateUnit = 'bytes' | 'items';

const rateFormats = new Map<string, Intl.NumberFormat>();

function rateFormat(locale: string | undefined, fractionDigits: number): Intl.NumberFormat {
  const key = `${locale ?? ''}|${fractionDigits}`;
  let format = rateFormats.get(key);
  if (!format) {
    format = new Intl.NumberFormat(locale, { maximumFractionDigits: fractionDigits });
    rateFormats.set(key, format);
  }
  return format;
}

/**
 * A transfer speed: "42 MB/s" for `bytes` (binary units, like `formatFileSize`), "85 items/s" for
 * `items`. An items rate keeps one decimal below 10 ("0.4 items/s", a slow hash of big files) and
 * groups the digits of a large one ("2,400 items/s"). An em dash when there is no usable rate.
 */
export function formatRate(
  perSecond: number | null | undefined,
  unit: RateUnit,
  locale?: string,
): string {
  if (typeof perSecond !== 'number' || !Number.isFinite(perSecond) || perSecond < 0) return EM_DASH;
  if (unit === 'bytes') return `${formatFileSize(perSecond, locale)}/s`;
  const rounded = perSecond >= 10 ? Math.round(perSecond) : Math.round(perSecond * 10) / 10;
  const text = rateFormat(locale, rounded >= 10 ? 0 : 1).format(rounded);
  return `${text} ${rounded === 1 ? 'item' : 'items'}/s`;
}

const MINUTE_SECS = 60;
const MAX_ETA_MINUTES = 24 * 60;

/**
 * The time a task still needs (`TaskDto.eta_secs`): "under 1 min", "about 3 min", "about 2 h
 * 10 min", and "more than a day" from 24 hours on (an estimate that long is noise). Rounded to the
 * nearest minute, so "about 60 min" never shows: it reads "about 1 h". A negative value counts as
 * zero, an infinite one as more than a day; an em dash when there is no estimate.
 */
export function formatEta(seconds: number | null | undefined): string {
  if (typeof seconds !== 'number' || Number.isNaN(seconds)) return EM_DASH;
  if (seconds < MINUTE_SECS) return 'under 1 min';
  const minutes = Number.isFinite(seconds) ? Math.round(seconds / MINUTE_SECS) : MAX_ETA_MINUTES;
  if (minutes >= MAX_ETA_MINUTES) return 'more than a day';
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  if (hours === 0) return `about ${rest} min`;
  return rest === 0 ? `about ${hours} h` : `about ${hours} h ${rest} min`;
}
