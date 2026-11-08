import { ApiError } from './http.ts';
import { isRecord, isUuid, type Obj } from './util.ts';

const ISO_DATE_RE =
  /^\d{4}-\d{2}-\d{2}(?:T\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:Z|[+-]\d{2}:\d{2}))?$/;

export const isIsoDate = (value: unknown): value is string =>
  typeof value === 'string' && ISO_DATE_RE.test(value) && !Number.isNaN(Date.parse(value));

interface Presence {
  required?: boolean;
}

interface Range extends Presence {
  min?: number;
  max?: number;
}

/**
 * Reads the fields of a request body and collects the messages Immich's validation pipe gives
 * (class-validator wording), so a client sees the text it would get from a real server. Every
 * reader returns `undefined` for an absent or invalid field; `nullable*` readers also return
 * `null` for an explicit `null`.
 */
export interface Checker {
  readonly errors: string[];
  readonly obj: Obj;
  /** Reports every key that is not in `allowed`. */
  only(allowed: readonly string[]): void;
  has(key: string): boolean;
  /** The nested object `key`, checked with the same collector. */
  child(key: string, opts?: Presence): Checker | undefined;
  /** The elements of the array `key`, each one checked as a nested object. */
  list(key: string, opts?: Presence): Checker[] | undefined;
  /** Reports a problem of field `key` that the typed readers do not cover. */
  fail(key: string, message: string): void;
  uuid(key: string, opts?: Presence): string | undefined;
  nullableUuid(key: string): string | null | undefined;
  uuidList(key: string, opts?: { max?: number; nonEmpty?: boolean }): string[];
  string(key: string, opts?: Presence & { nonEmpty?: boolean }): string | undefined;
  nullableString(key: string): string | null | undefined;
  stringList(key: string, opts?: Presence): string[] | undefined;
  boolean(key: string, opts?: Presence): boolean | undefined;
  integer(key: string, opts?: Range): number | undefined;
  nullableInteger(key: string, opts?: Range): number | null | undefined;
  date(key: string, opts?: Presence): string | undefined;
  nullableDate(key: string): string | null | undefined;
  oneOf<T extends string>(key: string, values: readonly T[], opts?: Presence): T | undefined;
  oneOfList<T extends string>(key: string, values: readonly T[]): T[] | undefined;
  /** Throws a 400 carrying every collected message. */
  done(): void;
}

export function createChecker(value: unknown, errors: string[] = [], prefix = ''): Checker {
  let obj: Obj = {};
  if (isRecord(value)) {
    obj = value;
  } else {
    errors.push(`${prefix === '' ? 'body' : prefix.replace(/\.$/, '')} must be an object`);
  }
  const path = (key: string) => `${prefix}${key}`;

  /** The one place that decides absent / null / invalid; `accept` returns undefined to refuse. */
  const scalar = <T>(
    key: string,
    opts: Presence & { nullable?: boolean },
    message: string,
    accept: (raw: unknown) => T | undefined,
    extra?: (accepted: T) => void,
  ): T | null | undefined => {
    const raw = obj[key];
    if (raw === undefined) {
      if (opts.required) {
        errors.push(`${path(key)} ${message}`);
      }
      return undefined;
    }
    if (raw === null && opts.nullable) {
      return null;
    }
    const accepted = accept(raw);
    if (accepted === undefined) {
      errors.push(`${path(key)} ${message}`);
      return undefined;
    }
    extra?.(accepted);
    return accepted;
  };

  const acceptString = (raw: unknown) => (typeof raw === 'string' ? raw : undefined);
  const acceptInteger = (raw: unknown) =>
    typeof raw === 'number' && Number.isInteger(raw) ? raw : undefined;
  const acceptUuid = (raw: unknown) => (isUuid(raw) ? raw : undefined);
  const acceptDate = (raw: unknown) => (isIsoDate(raw) ? raw : undefined);
  const checkRange = (key: string, opts: Range) => (n: number) => {
    if (opts.min !== undefined && n < opts.min) {
      errors.push(`${path(key)} must not be less than ${opts.min}`);
    }
    if (opts.max !== undefined && n > opts.max) {
      errors.push(`${path(key)} must not be greater than ${opts.max}`);
    }
  };
  const DATE_MESSAGE = 'must be a valid ISO 8601 date string';
  const enumMessage = (values: readonly string[]) =>
    `must be one of the following values: ${values.join(', ')}`;

  return {
    errors,
    obj,
    only(allowed) {
      for (const key of Object.keys(obj)) {
        if (!allowed.includes(key)) {
          errors.push(`property ${path(key)} should not exist`);
        }
      }
    },
    has: (key) => obj[key] !== undefined,
    child(key, opts) {
      if (obj[key] === undefined) {
        if (opts?.required) {
          errors.push(`${path(key)} must be an object`);
        }
        return undefined;
      }
      return createChecker(obj[key], errors, `${path(key)}.`);
    },
    list(key, opts) {
      const raw = obj[key];
      if (raw === undefined) {
        if (opts?.required) {
          errors.push(`${path(key)} must be an array`);
        }
        return undefined;
      }
      if (!Array.isArray(raw)) {
        errors.push(`${path(key)} must be an array`);
        return undefined;
      }
      return raw.map((item, index) => createChecker(item, errors, `${path(key)}[${index}].`));
    },
    fail(key, message) {
      errors.push(`${path(key)} ${message}`);
    },
    uuid: (key, opts = {}) => scalar(key, opts, 'must be a UUID', acceptUuid) ?? undefined,
    nullableUuid: (key) => scalar(key, { nullable: true }, 'must be a UUID', acceptUuid),
    uuidList(key, opts) {
      const raw = obj[key];
      if (!Array.isArray(raw)) {
        errors.push(`${path(key)} must be an array`);
        return [];
      }
      if (raw.length === 0 && opts?.nonEmpty) {
        errors.push(`${path(key)} should not be empty`);
      }
      if (opts?.max !== undefined && raw.length > opts.max) {
        errors.push(`${path(key)} must contain no more than ${opts.max} elements`);
      }
      const ids: string[] = [];
      raw.forEach((item, index) => {
        if (isUuid(item)) {
          ids.push(item);
        } else {
          errors.push(`${path(key)}.${index} must be a UUID`);
        }
      });
      return ids;
    },
    string(key, opts = {}) {
      return (
        scalar(key, opts, 'must be a string', acceptString, (text) => {
          if (opts.nonEmpty && text.length === 0) {
            errors.push(`${path(key)} should not be empty`);
          }
        }) ?? undefined
      );
    },
    nullableString: (key) => scalar(key, { nullable: true }, 'must be a string', acceptString),
    stringList(key, opts) {
      const raw = obj[key];
      if (raw === undefined) {
        if (opts?.required) {
          errors.push(`${path(key)} must be an array`);
        }
        return undefined;
      }
      if (!Array.isArray(raw)) {
        errors.push(`${path(key)} must be an array`);
        return undefined;
      }
      const items: string[] = [];
      raw.forEach((item, index) => {
        if (typeof item === 'string') {
          items.push(item);
        } else {
          errors.push(`${path(key)}.${index} must be a string`);
        }
      });
      return items;
    },
    boolean: (key, opts = {}) =>
      scalar(key, opts, 'must be a boolean value', (raw) =>
        typeof raw === 'boolean' ? raw : undefined,
      ) ?? undefined,
    integer: (key, opts = {}) =>
      scalar(key, opts, 'must be an integer number', acceptInteger, checkRange(key, opts)) ??
      undefined,
    nullableInteger: (key, opts = {}) =>
      scalar(
        key,
        { ...opts, nullable: true },
        'must be an integer number',
        acceptInteger,
        checkRange(key, opts),
      ),
    date: (key, opts = {}) => scalar(key, opts, DATE_MESSAGE, acceptDate) ?? undefined,
    nullableDate: (key) => scalar(key, { nullable: true }, DATE_MESSAGE, acceptDate),
    oneOf: (key, values, opts = {}) =>
      scalar(key, opts, enumMessage(values), (raw) => values.find((v) => v === raw)) ?? undefined,
    oneOfList: <T extends string>(key: string, values: readonly T[]) => {
      const raw = obj[key];
      if (raw === undefined) {
        return undefined;
      }
      if (!Array.isArray(raw)) {
        errors.push(`${path(key)} must be an array`);
        return undefined;
      }
      const items: T[] = [];
      raw.forEach((item, index) => {
        const found = values.find((v) => v === item);
        if (found === undefined) {
          errors.push(`${path(key)}.${index} ${enumMessage(values)}`);
        } else {
          items.push(found);
        }
      });
      return items;
    },
    done() {
      if (errors.length > 0) {
        throw new ApiError(400, [...errors]);
      }
    },
  };
}
