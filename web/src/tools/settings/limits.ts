/**
 * The limits the server enforces on the settings (the `SettingsDto` constants in
 * `crates/opm-core/src/dto/settings.rs`). They are mirrored here only to tell the user about a bad
 * value before Save. The server is authoritative: never be stricter than it, and show its answer
 * (a 422 `validation_failed`) whenever the two disagree.
 */

export interface NumberRange {
  readonly min: number;
  readonly max: number;
}

/** Longest file or folder name that generated names may have, in UTF-8 bytes. */
export const NAME_MAX_LEN: NumberRange = { min: 16, max: 255 };
/** Encrypted shares (eCryptfs) only allow names of this many bytes. */
export const ENCRYPTED_SHARE_NAME_LEN = 143;

export const SCAN_CONCURRENCY: NumberRange = { min: 1, max: 16 };
export const HASH_CONCURRENCY: NumberRange = { min: 1, max: 8 };
export const THUMBNAIL_CONCURRENCY: NumberRange = { min: 1, max: 8 };

/** Minutes between full rescans. `0` is also valid and means never. */
export const RESCAN_INTERVAL_MINUTES: NumberRange = { min: 5, max: 10_080 };
export const RESCAN_NEVER = 0;

/** Days before trashed items are deleted. The setting is `null` when they are kept for good. */
export const TRASH_RETENTION_DAYS: NumberRange = { min: 1, max: 3_650 };
/** What turning the automatic deletion on starts with when nothing was chosen before. */
export const DEFAULT_TRASH_RETENTION_DAYS = 30;

export const MAX_SIDECAR_EXTENSIONS = 50;
/** Characters of one extension: lowercase letters and digits, without the dot. */
export const MAX_SIDECAR_EXTENSION_LEN = 10;
/** Entries of each of the two junk lists. */
export const MAX_JUNK_ENTRIES = 200;
/** UTF-8 bytes of one junk name or prefix. */
export const MAX_JUNK_ENTRY_BYTES = 255;

export function isInRange(value: number, range: NumberRange): boolean {
  return Number.isInteger(value) && value >= range.min && value <= range.max;
}

/** `value` is a whole number of minutes the server accepts: 0 (never) or inside the range. */
export function isValidRescanInterval(value: number): boolean {
  return value === RESCAN_NEVER || isInRange(value, RESCAN_INTERVAL_MINUTES);
}

const utf8 = new TextEncoder();

export function utf8Length(value: string): number {
  return utf8.encode(value).length;
}

const EXTENSION_PATTERN = new RegExp(`^[a-z0-9]{1,${MAX_SIDECAR_EXTENSION_LEN}}$`);

/**
 * What the server does to a sidecar extension before checking it: trim, drop one leading dot,
 * lowercase ASCII letters. Blank means "nothing to add".
 */
export function normalizeExtension(raw: string): string {
  const trimmed = raw.trim();
  const bare = trimmed.startsWith('.') ? trimmed.slice(1) : trimmed;
  return bare.replace(/[A-Z]/g, (letter) => letter.toLowerCase());
}

/** The reason a normalised extension is refused (a predicate: `"x.y" must be ...`), else `null`. */
export function validateExtension(extension: string): string | null {
  return EXTENSION_PATTERN.test(extension)
    ? null
    : `must be 1 to ${MAX_SIDECAR_EXTENSION_LEN} letters or digits, without a dot`;
}

/** What the server does to a junk name or prefix before checking it: trim and compose (NFC). */
export function normalizeJunkEntry(raw: string): string {
  return raw.trim().normalize('NFC');
}

function validateJunkEntry(entry: string, rejectDotNames: boolean): string | null {
  if (entry.includes('/') || entry.includes('\0')) return 'must not contain / or NUL';
  if (rejectDotNames && (entry === '.' || entry === '..')) return 'must not be . or ..';
  if (utf8Length(entry) > MAX_JUNK_ENTRY_BYTES) {
    return `is too long (at most ${MAX_JUNK_ENTRY_BYTES} bytes)`;
  }
  return null;
}

/** The reason a normalised junk name is refused, else `null`. */
export function validateJunkName(entry: string): string | null {
  return validateJunkEntry(entry, true);
}

/** Like a name, but `.` is fine: it matches every hidden file. */
export function validateJunkPrefix(entry: string): string | null {
  return validateJunkEntry(entry, false);
}
