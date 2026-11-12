import { render } from 'lit';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import type { FileItem } from '@/api/generated/FileItem';
import {
  INFO_ROWS,
  type InfoContext,
  type InfoRow,
  renderInfoTable,
  viewerInfoOpen,
} from '@/components/viewer-info';
import { loadVolumes } from '@/stores/volumes';
import { fileDetail, fileItem } from '@/test/file-item';
import { EM_DASH, formatCaptureDate, formatFileSize, formatFileTime } from '@/util/format';

const hosts: HTMLElement[] = [];

function table(overrides: Partial<InfoContext> = {}): HTMLElement {
  const host = document.createElement('div');
  document.body.append(host);
  hosts.push(host);
  render(
    renderInfoTable({ item: fileItem(), detail: undefined, pending: false, ...overrides }),
    host,
  );
  return host;
}

function withDetail(item: FileItem, detail = fileDetail(item)): HTMLElement {
  return table({ item, detail });
}

const row = (host: HTMLElement, id: string) =>
  host.querySelector<HTMLElement>(`[data-info-row="${id}"]`);
const text = (host: HTMLElement, id: string) =>
  row(host, id)?.querySelector('td')?.textContent?.replace(/\s+/g, ' ').trim();
const ids = (host: HTMLElement) =>
  [...host.querySelectorAll('[data-info-row]')].map((tr) => tr.getAttribute('data-info-row'));

beforeEach(async () => {
  vi.spyOn(api, 'get').mockResolvedValue([{ id: 1, name: 'photos', writable: true, online: true }]);
  await loadVolumes();
});

afterEach(() => {
  for (const host of hosts.splice(0)) host.remove();
  vi.restoreAllMocks();
  vi.resetModules();
  localStorage.clear();
});

describe('INFO_ROWS', () => {
  it('has unique ids, in display order', () => {
    const rowIds = INFO_ROWS.map((entry) => entry.id);
    expect(new Set(rowIds).size).toBe(rowIds.length);
    expect(rowIds).toEqual([
      'name',
      'location',
      'size',
      'kind',
      'dimensions',
      'duration',
      'captured',
      'camera',
      'rating',
      'flag',
      'tags',
      'sidecars',
      'hash',
      'modified',
      'created',
      'immich',
    ]);
  });

  it('is the one place to add a line', () => {
    const extra: InfoRow = { id: 'extra', label: 'Extra', value: ({ item }) => `#${item.id}` };
    (INFO_ROWS as InfoRow[]).push(extra);
    try {
      expect(text(table({ item: fileItem({ id: 9 }) }), 'extra')).toBe('#9');
    } finally {
      (INFO_ROWS as InfoRow[]).pop();
    }
  });
});

describe('renderInfoTable', () => {
  it('shows a row per entry that has something to say, with its label', () => {
    const host = withDetail(fileItem());
    expect(ids(host)).toEqual([
      'name',
      'location',
      'size',
      'kind',
      'dimensions',
      'captured',
      'camera',
      'rating',
      'flag',
      'tags',
      'sidecars',
      'hash',
      'modified',
      'created',
    ]);
    expect(host.querySelector('[data-info-row="name"] th')?.textContent?.trim()).toBe('Name');
    expect(host.querySelector('table')?.classList.contains('table-sm')).toBe(true);
  });

  it('shows the name', () => {
    expect(text(table({ item: fileItem({ name: 'Holiday 1.jpg' }) }), 'name')).toBe(
      'Holiday 1.jpg',
    );
  });

  it('resolves the volume name and adds the folder', () => {
    expect(text(table({ item: fileItem({ volume_id: 1, dir: 'trip/day 1' }) }), 'location')).toBe(
      'photos/trip/day 1',
    );
    expect(text(table({ item: fileItem({ volume_id: 1, dir: '' }) }), 'location')).toBe('photos');
  });

  it('falls back to the volume id when the volume is not known', () => {
    expect(text(table({ item: fileItem({ volume_id: 9, dir: 'a' }) }), 'location')).toBe(
      'Volume 9/a',
    );
  });

  it('formats the size and gives the exact bytes in a tooltip', () => {
    const host = table({ item: fileItem({ size: 4_200_000 }) });
    expect(text(host, 'size')).toBe(formatFileSize(4_200_000));
    expect(row(host, 'size')?.querySelector('span')?.getAttribute('title')).toBe(
      `${(4_200_000).toLocaleString()} bytes`,
    );
  });

  it('shows the kind, and the mime type once the detail is there', () => {
    expect(text(table({ item: fileItem({ kind: 'video' }) }), 'kind')).toBe('Video');
    expect(text(withDetail(fileItem()), 'kind')).toBe('Image \u00b7 image/jpeg');
    const noMime = fileDetail(fileItem(), { mime: null });
    expect(text(withDetail(fileItem(), noMime), 'kind')).toBe('Image');
  });

  it('shows the dimensions and the megapixels', () => {
    expect(text(table(), 'dimensions')).toBe('6000 \u00d7 4000 \u00b7 24.0 MP');
    expect(text(table({ item: fileItem({ width: 100, height: 80 }) }), 'dimensions')).toBe(
      '100 \u00d7 80',
    );
    expect(text(table({ item: fileItem({ width: null, height: null }) }), 'dimensions')).toBe(
      EM_DASH,
    );
  });

  it('shows the duration of a video or audio only', () => {
    const video = fileItem({ kind: 'video', duration_ms: 65_000 });
    expect(text(table({ item: video }), 'duration')).toBe('1:05');
    expect(row(table(), 'duration')).toBeNull();
  });

  it('shows the capture date with where it comes from', () => {
    const ts = Date.UTC(2024, 0, 5, 15, 4) / 1000;
    const exif = table({ item: fileItem({ capture_ts: ts, capture_src: 'exif' }) });
    expect(text(exif, 'captured')).toBe(`${formatCaptureDate(ts)} EXIF`);
    const labels = { exif: 'EXIF', xmp: 'XMP', video: 'Video', fs: 'File time' } as const;
    for (const [source, label] of Object.entries(labels)) {
      const item = fileItem({ capture_ts: ts, capture_src: source as keyof typeof labels });
      expect(text(table({ item }), 'captured')).toBe(`${formatCaptureDate(ts)} ${label}`);
    }
  });

  it('shows a dash for a file without a capture date', () => {
    const host = table({ item: fileItem({ capture_ts: null, capture_src: null }) });
    expect(text(host, 'captured')).toBe(EM_DASH);
    expect(row(host, 'captured')?.querySelector('.badge')).toBeNull();
  });

  it('shows the camera, or a dash', () => {
    expect(text(table(), 'camera')).toBe('Canon EOS R5');
    expect(text(table({ item: fileItem({ camera: null }) }), 'camera')).toBe(EM_DASH);
  });

  it('shows the rating as read-only stars', () => {
    const host = table({ item: fileItem({ rating: 3 }) });
    const rating = row(host, 'rating')?.querySelector('.rating');
    expect(rating?.getAttribute('role')).toBe('img');
    expect(rating?.getAttribute('aria-label')).toBe('3 out of 5 stars');
    const stars = [...(rating?.children ?? [])];
    expect(stars).toHaveLength(5);
    expect(stars.map((star) => star.getAttribute('aria-current'))).toEqual([
      null,
      null,
      'true',
      null,
      null,
    ]);
    expect(row(host, 'rating')?.querySelector('input')).toBeNull();
  });

  it('says when a file is not rated, and keeps odd ratings in range', () => {
    expect(text(table({ item: fileItem({ rating: 0 }) }), 'rating')).toBe('Not rated');
    expect(text(table({ item: fileItem({ rating: -2 }) }), 'rating')).toBe('Not rated');
    const high = row(table({ item: fileItem({ rating: 9 }) }), 'rating')?.querySelector('.rating');
    expect(high?.getAttribute('aria-label')).toBe('5 out of 5 stars');
  });

  it.each([
    [1, 'Selected', 'badge-success'],
    [-1, 'Rejected', 'badge-error'],
  ])('shows flag %i', (flag, label, badge) => {
    const host = table({ item: fileItem({ flag }) });
    expect(text(host, 'flag')).toBe(label);
    expect(row(host, 'flag')?.querySelector('.badge')?.classList.contains(badge)).toBe(true);
  });

  it('says none for an unflagged file', () => {
    expect(text(table({ item: fileItem({ flag: 0 }) }), 'flag')).toBe('None');
  });

  it('shows the tags as badges', () => {
    const host = withDetail(fileItem(), fileDetail(fileItem(), { tags: ['trip', 'family'] }));
    const badges = [...(row(host, 'tags')?.querySelectorAll('.badge') ?? [])];
    expect(badges.map((badge) => badge.textContent?.trim())).toEqual(['trip', 'family']);
    expect(text(withDetail(fileItem()), 'tags')).toBe(EM_DASH);
  });

  it('shows the sidecars with how they are linked', () => {
    const detail = fileDetail(fileItem(), {
      sidecars: [
        { name: 'IMG_0007.jpg.xmp', kind: 'exact' },
        { name: 'IMG_0007.xmp', kind: 'stem' },
      ],
    });
    const host = withDetail(fileItem(), detail);
    const entries = [...(row(host, 'sidecars')?.querySelectorAll('li') ?? [])];
    expect(entries.map((entry) => entry.textContent?.replace(/\s+/g, ' ').trim())).toEqual([
      'IMG_0007.jpg.xmp exact',
      'IMG_0007.xmp stem',
    ]);
  });

  it('truncates the hash, in monospace, with the whole value in a tooltip', () => {
    const hash = 'da39a3ee5e6b4b0d3255bfef95601890afd80709';
    const host = withDetail(fileItem(), fileDetail(fileItem(), { hash }));
    const span = row(host, 'hash')?.querySelector('span');
    expect(span?.textContent).toBe('da39a3ee5e6b\u2026');
    expect(span?.classList.contains('font-mono')).toBe(true);
    expect(span?.getAttribute('title')).toBe(hash);
  });

  it('says when the hash is not computed yet', () => {
    expect(text(withDetail(fileItem()), 'hash')).toBe('not computed yet');
  });

  it('shows the modification and the creation time', () => {
    const item = fileItem({ mtime: 1_700_000_000 });
    const detail = fileDetail(item, { btime: 1_600_000_000 });
    const host = withDetail(item, detail);
    expect(text(host, 'modified')).toBe(formatFileTime(1_700_000_000));
    expect(text(host, 'created')).toBe(formatFileTime(1_600_000_000));
    expect(text(withDetail(item), 'created')).toBe(EM_DASH);
  });

  it('shows an In Immich badge only for a file Immich lists', () => {
    expect(text(table({ item: fileItem({ in_immich: true }) }), 'immich')).toBe('In Immich');
    expect(row(table({ item: fileItem({ in_immich: false }) }), 'immich')).toBeNull();
  });

  it('shows skeletons for what only the detail has while it loads', () => {
    const host = table({ pending: true });
    for (const id of ['tags', 'sidecars', 'hash', 'created']) {
      expect(row(host, id)?.querySelector('.skeleton'), id).not.toBeNull();
    }
    expect(row(host, 'name')?.querySelector('.skeleton')).toBeNull();
    expect(text(host, 'modified')).not.toBe('');
  });

  it('shows dashes for what only the detail has when it could not be loaded', () => {
    const host = table({ pending: false });
    for (const id of ['tags', 'sidecars', 'hash', 'created']) {
      expect(text(host, id), id).toBe(EM_DASH);
    }
    expect(host.querySelector('.skeleton')).toBeNull();
  });

  it('never touches the clipboard', () => {
    const clipboard = vi.fn();
    vi.stubGlobal('navigator', { ...navigator, clipboard: { writeText: clipboard } });
    withDetail(fileItem(), fileDetail(fileItem(), { hash: 'a'.repeat(40) }));
    expect(clipboard).not.toHaveBeenCalled();
    vi.unstubAllGlobals();
  });
});

describe('viewerInfoOpen', () => {
  it('starts closed', () => {
    expect(viewerInfoOpen.value).toBe(false);
  });

  it('is stored under the app prefix, and restored by the next page load', async () => {
    viewerInfoOpen.value = true;
    expect(localStorage.getItem('opm.v1.viewerInfoOpen')).toBe('true');

    vi.resetModules();
    const fresh = await import('@/components/viewer-info');
    expect(fresh.viewerInfoOpen.value).toBe(true);
    fresh.viewerInfoOpen.dispose();
    viewerInfoOpen.value = false;
  });

  it('ignores a stored value that is not a boolean', async () => {
    localStorage.setItem('opm.v1.viewerInfoOpen', '"yes"');
    vi.resetModules();
    const fresh = await import('@/components/viewer-info');
    expect(fresh.viewerInfoOpen.value).toBe(false);
    fresh.viewerInfoOpen.dispose();
  });
});
