import { describe, expect, it } from 'vitest';
import { sampleStatistics } from '@/test/statistics-fixtures';
import { dailyRows, localDay } from '@/tools/statistics/statistics-data';

describe('statistics daily data', () => {
  it('keeps absent observations distinct from an observed day with no additions', () => {
    const rows = dailyRows(sampleStatistics());
    expect(rows).toHaveLength(3);
    expect(rows[0]?.files).toBeNull();
    expect(rows[0]?.state).toBe('No observation');
    expect(rows[2]?.files).toBe(10);
    expect(rows[2]?.activity.duplicates).toEqual({ count: 2, bytes: 2048 });
    expect(rows[2]?.net).toBeNull();
  });

  it('sums observations across volumes without crossing timezone segments', () => {
    const data = sampleStatistics();
    const point = data.snapshots[0];
    if (!point) throw new Error('Missing fixture');
    data.snapshots.push({ ...point, inventory: { ...point.inventory, volume_id: 2 } });
    data.snapshots.push({
      ...point,
      segment_id: 2,
      timezone: 'UTC',
      inventory: { ...point.inventory, files: 99 },
    });
    const rows = dailyRows(data).filter((row) => row.date === '2026-11-13');
    expect(rows.map((row) => row.files)).toEqual([20, 99]);
    expect(rows.every((row) => row.net === null)).toBe(true);
  });

  it('uses the reporting timezone rather than the browser day, including DST', () => {
    expect(localDay(Date.parse('2026-11-13T07:59:59Z') / 1000, 'America/Los_Angeles')).toBe(
      '2026-11-12',
    );
    expect(localDay(Date.parse('2026-11-13T08:00:00Z') / 1000, 'America/Los_Angeles')).toBe(
      '2026-11-13',
    );
  });

  it('does not merge a later return to the same timezone or claim full coverage for a split day', () => {
    const data = sampleStatistics();
    const point = data.snapshots[0];
    if (!point) throw new Error('Missing fixture');
    const segment = data.segments[0];
    if (!segment) throw new Error('Missing fixture segment');
    data.segments.push({ ...segment, id: 2, started_at: point.observed_at });
    data.snapshots.push({ ...point, segment_id: 2, inventory: { ...point.inventory, files: 11 } });
    data.coverage = [{ started_at: 1700000000, last_seen_at: 1900000000, stopped_at: null }];
    const rows = dailyRows(data).filter((row) => row.date === point.day);
    expect(rows.map((row) => row.files)).toEqual([10, 11]);
    expect(rows.every((row) => row.net === null)).toBe(true);
  });
});
