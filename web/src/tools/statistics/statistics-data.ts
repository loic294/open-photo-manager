import type { StatisticsInventory } from '@/api/generated/StatisticsInventory';
import type { StatisticsResponse } from '@/api/generated/StatisticsResponse';

export const ACTIVITIES = [
  ['added', 'Added'],
  ['modified', 'Modified'],
  ['removed', 'Removed from index'],
  ['duplicates', 'Duplicates trashed'],
  ['duplicate_sidecars', 'Duplicate sidecars trashed'],
  ['trashed', 'All files trashed'],
  ['restored', 'Restored'],
  ['purged', 'Permanently deleted'],
  ['baseline', 'Initial inventory'],
] as const;
export type Activity = (typeof ACTIVITIES)[number][0];

export interface Day {
  segment: number;
  date: string;
  timezone: string;
  observedAt: number | null;
  files: number | null;
  bytes: number | null;
  net: number | null;
  inventory: StatisticsInventory[];
  activity: Record<string, { count: number; bytes: number }>;
  state: 'No observation' | 'Partial' | 'Today · provisional' | 'Covered';
}

export function localDay(at: number, timeZone: string): string {
  const parts = new Intl.DateTimeFormat('en-CA', {
    timeZone,
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
  }).formatToParts(new Date(at * 1000));
  const part = (type: string) => parts.find((value) => value.type === type)?.value ?? '';
  return `${part('year')}-${part('month')}-${part('day')}`;
}

export function dailyRows(data: StatisticsResponse): Day[] {
  const grouped = new Map<string, Day>();
  const get = (date: string, timezone: string, segment: number) => {
    const key = `${date}|${segment}`;
    let row = grouped.get(key);
    if (!row) {
      row = {
        segment,
        date,
        timezone,
        observedAt: null,
        files: null,
        bytes: null,
        net: null,
        inventory: [],
        activity: {},
        state: 'No observation',
      };
      grouped.set(key, row);
    }
    return row;
  };
  for (const point of data.snapshots) {
    const row = get(point.day, point.timezone, point.segment_id);
    row.inventory.push(point.inventory);
    row.files = (row.files ?? 0) + point.inventory.files;
    row.bytes = (row.bytes ?? 0) + point.inventory.bytes;
    row.observedAt = Math.max(row.observedAt ?? 0, point.observed_at);
  }
  for (const point of data.activity) {
    const row = get(point.day, point.timezone, point.segment_id);
    row.activity[point.action] ??= { count: 0, bytes: 0 };
    const value = row.activity[point.action];
    value.count += point.count;
    value.bytes += point.bytes;
  }
  for (
    let date = new Date(`${data.from}T00:00:00Z`);
    date.toISOString().slice(0, 10) <= data.to;
    date = new Date(date.getTime() + 86_400_000)
  ) {
    const day = date.toISOString().slice(0, 10);
    if (![...grouped.values()].some((row) => row.date === day))
      get(day, data.timezone, data.segments.at(-1)?.id ?? 0);
  }
  const rows = [...grouped.values()].sort(
    (a, b) => a.date.localeCompare(b.date) || a.segment - b.segment,
  );
  for (const [index, row] of rows.entries()) {
    const observed = row.observedAt !== null || Object.keys(row.activity).length > 0;
    if (!observed) continue;
    const today = localDay(Date.now() / 1000, row.timezone);
    const segment = data.segments.find((value) => value.id === row.segment);
    const wholeDay =
      segment !== undefined &&
      localDay(segment.started_at, row.timezone) < row.date &&
      (segment.ended_at === null || localDay(segment.ended_at, row.timezone) > row.date);
    const covered =
      wholeDay &&
      data.coverage.some(
        (period) =>
          localDay(period.started_at, row.timezone) < row.date &&
          localDay(period.last_seen_at, row.timezone) > row.date,
      );
    row.state = row.date === today ? 'Today · provisional' : covered ? 'Covered' : 'Partial';
    const previous = rows[index - 1];
    if (
      previous?.files !== null &&
      previous?.files !== undefined &&
      row.files !== null &&
      previous.timezone === row.timezone &&
      previous.segment === row.segment &&
      previous.inventory.every((volume) => volume.baseline_complete) &&
      row.inventory.every((volume) => volume.baseline_complete) &&
      previous.inventory.map((volume) => volume.volume_id).join() ===
        row.inventory.map((volume) => volume.volume_id).join()
    ) {
      row.net = row.files - previous.files;
    }
  }
  return rows;
}

export function latestInventory(rows: readonly Day[]): StatisticsInventory[] {
  return rows.findLast((row) => row.inventory.length > 0)?.inventory ?? [];
}
