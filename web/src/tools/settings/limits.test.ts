import { describe, expect, it } from 'vitest';
import {
  isInRange,
  isValidRescanInterval,
  NAME_MAX_LEN,
  normalizeExtension,
  normalizeJunkEntry,
  SCAN_CONCURRENCY,
  utf8Length,
  validateExtension,
  validateJunkName,
  validateJunkPrefix,
} from '@/tools/settings/limits';

describe('ranges', () => {
  it('accept whole numbers inside the limits, both ends included', () => {
    expect(isInRange(16, NAME_MAX_LEN)).toBe(true);
    expect(isInRange(255, NAME_MAX_LEN)).toBe(true);
    expect(isInRange(15, NAME_MAX_LEN)).toBe(false);
    expect(isInRange(256, NAME_MAX_LEN)).toBe(false);
    expect(isInRange(2.5, SCAN_CONCURRENCY)).toBe(false);
    expect(isInRange(Number.NaN, SCAN_CONCURRENCY)).toBe(false);
  });

  it('allow 0 (never) or 5 to 10080 minutes between rescans', () => {
    expect(isValidRescanInterval(0)).toBe(true);
    expect(isValidRescanInterval(5)).toBe(true);
    expect(isValidRescanInterval(10_080)).toBe(true);
    expect(isValidRescanInterval(1)).toBe(false);
    expect(isValidRescanInterval(4)).toBe(false);
    expect(isValidRescanInterval(10_081)).toBe(false);
    expect(isValidRescanInterval(Number.NaN)).toBe(false);
  });
});

describe('sidecar extensions', () => {
  it('are trimmed, lose one leading dot and are lowercased like the server does', () => {
    expect(normalizeExtension('  .XMP ')).toBe('xmp');
    expect(normalizeExtension('Json')).toBe('json');
    expect(normalizeExtension('..xmp')).toBe('.xmp');
    expect(normalizeExtension('.')).toBe('');
    expect(normalizeExtension('   ')).toBe('');
  });

  it('lowercase ASCII only, so a look-alike letter stays invalid', () => {
    const kelvin = '\u212A';

    expect(normalizeExtension(kelvin)).toBe(kelvin);
    expect(validateExtension(normalizeExtension(kelvin))).not.toBeNull();
  });

  it('must be 1 to 10 lowercase letters or digits', () => {
    expect(validateExtension('xmp')).toBeNull();
    expect(validateExtension('mp4')).toBeNull();
    expect(validateExtension('a'.repeat(10))).toBeNull();
    expect(validateExtension('a'.repeat(11))).toBe(
      'must be 1 to 10 letters or digits, without a dot',
    );
    expect(validateExtension('tar.gz')).not.toBeNull();
    expect(validateExtension('a b')).not.toBeNull();
    expect(validateExtension('é')).not.toBeNull();
    expect(validateExtension('')).not.toBeNull();
    expect(validateExtension('xmp\n')).not.toBeNull();
  });
});

describe('junk names and prefixes', () => {
  it('are trimmed and composed (NFC)', () => {
    expect(normalizeJunkEntry('  .DS_Store ')).toBe('.DS_Store');
    expect(normalizeJunkEntry('e\u0301')).toBe('\u00e9');
    expect(normalizeJunkEntry(' \t ')).toBe('');
  });

  it('may not contain a slash or NUL', () => {
    expect(validateJunkName('a/b')).toBe('must not contain / or NUL');
    expect(validateJunkName('a\0b')).toBe('must not contain / or NUL');
    expect(validateJunkPrefix('a/b')).toBe('must not contain / or NUL');
    expect(validateJunkName('Thumbs.db')).toBeNull();
    expect(validateJunkName('Photo Booth Library')).toBeNull();
  });

  it('names may not be . or .., prefixes may', () => {
    expect(validateJunkName('.')).toBe('must not be . or ..');
    expect(validateJunkName('..')).toBe('must not be . or ..');
    expect(validateJunkName('...')).toBeNull();
    expect(validateJunkPrefix('.')).toBeNull();
    expect(validateJunkPrefix('._')).toBeNull();
  });

  it('are limited to 255 UTF-8 bytes, not characters', () => {
    expect(validateJunkName('a'.repeat(255))).toBeNull();
    expect(validateJunkName('a'.repeat(256))).toBe('is too long (at most 255 bytes)');
    expect(validateJunkName('é'.repeat(127))).toBeNull();
    expect(validateJunkName('é'.repeat(128))).toBe('is too long (at most 255 bytes)');
    expect(utf8Length('é')).toBe(2);
    expect(utf8Length('😀')).toBe(4);
  });
});
