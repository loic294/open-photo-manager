const COUNT_FORMAT = new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 });

/** A count with the thousands separators of the user's locale: `1234567` -> `1,234,567`. */
export function formatCount(n: number): string {
  return COUNT_FORMAT.format(n);
}
