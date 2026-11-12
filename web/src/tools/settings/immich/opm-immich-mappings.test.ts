import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { ApiError } from '@/api/errors';
import type { ImmichStatusDto } from '@/api/generated/ImmichStatusDto';
import { applyImmichStatus, immichStatus } from '@/stores/immich';
import { loadVolumes } from '@/stores/volumes';
import { mount } from '@/test/helpers';
import { sampleImmichStatus, sampleLibraries } from '@/test/immich-fixtures';
import '@/tools/settings/immich/opm-immich-mappings';

type Card = HTMLElementTagNameMap['opm-immich-mappings'];
const button = (el: Element, text: string) =>
  [...el.querySelectorAll('button')].find((item) => item.textContent?.trim() === text);
const row = (el: Card, id: number) => {
  const result = el.querySelector(`[data-volume="${id}"]`);
  if (!result) throw new Error(`Missing volume ${id}`);
  return result;
};
async function choose(el: Card, id: number, value: string) {
  row(el, id)
    .querySelector('opm-select')
    ?.dispatchEvent(new CustomEvent('select-change', { detail: { value }, bubbles: true }));
  await el.updateComplete;
  await el.querySelector('opm-save-bar')?.updateComplete;
}
async function prefix(el: Card, id: number, value: string) {
  const input = row(el, id).querySelector('input');
  if (!input) throw new Error('Missing prefix');
  input.value = value;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await el.updateComplete;
  await el.querySelector('opm-save-bar')?.updateComplete;
}
async function open(status: ImmichStatusDto = sampleImmichStatus()) {
  applyImmichStatus(status);
  const el = await mount('opm-immich-mappings');
  if (status.mode === 'full' && !status.needs_reconnect) {
    await vi.waitFor(() => expect(api.get).toHaveBeenCalledWith('/api/immich/libraries'));
    await el.updateComplete;
    for (const select of el.querySelectorAll('opm-select')) await select.updateComplete;
  }
  return el;
}

beforeEach(async () => {
  vi.spyOn(api, 'get').mockImplementation(async (path) => {
    if (path === '/api/volumes')
      return [
        { id: 1, name: 'photos', online: true, writable: true },
        { id: 2, name: 'archive', online: false, writable: true },
        { id: 3, name: 'other', online: true, writable: true },
      ];
    if (path === '/api/immich/libraries') return { libraries: sampleLibraries, limited: false };
    return [];
  });
  vi.spyOn(api, 'post').mockResolvedValue({});
  vi.spyOn(api, 'put').mockResolvedValue({ mappings: [] });
  await loadVolumes();
});
afterEach(() => vi.restoreAllMocks());

it('lists online and offline volumes and pre-fills only a single import path', async () => {
  const el = await open();
  expect(el.textContent).toContain('Offline');
  expect(row(el, 1).querySelector('select')).not.toBeNull();
  expect(row(el, 1).querySelector('select')?.textContent).toContain('Photos');
  await choose(el, 1, 'photos-lib');
  expect(row(el, 1).querySelector('input')?.value).toBe('/mnt/photos');
  await choose(el, 2, 'archive-lib');
  expect(row(el, 2).querySelector('input')?.value).toBe('');
  expect(button(el, 'Save')?.disabled).toBe(true);
  expect(row(el, 2).textContent).toContain('Choose a library and enter a prefix');
});

it('sends the complete set in volume row order and starts sync after saving', async () => {
  const mappings = [
    {
      volume_id: 2,
      volume_name: 'archive',
      library_id: 'archive-lib',
      library_name: 'Archive',
      path_prefix: '/mnt/archive',
    },
  ];
  vi.mocked(api.put).mockResolvedValue({ mappings });
  const el = await open();
  await choose(el, 2, 'archive-lib');
  await prefix(el, 2, '/mnt/archive');
  button(el, 'Save')?.click();
  await vi.waitFor(() =>
    expect(api.post).toHaveBeenCalledWith('/api/immich/sync', { body: { full: false } }),
  );
  expect(api.put).toHaveBeenCalledWith('/api/immich/mappings', {
    body: {
      mappings: [{ volume_id: 2, library_id: 'archive-lib', path_prefix: '/mnt/archive' }],
    },
  });
  expect(immichStatus.value?.mappings).toEqual(mappings);
  expect(el.querySelector('opm-save-bar')).toBeNull();
});

it('maps validation indices to SENT rows, skipping the first unmapped visible volume', async () => {
  vi.mocked(api.put).mockRejectedValue(
    new ApiError({
      status: 422,
      code: 'validation_failed',
      message: 'Refused',
      fields: [{ field: 'mappings[1].path_prefix', message: 'Overlap in third row' }],
    }),
  );
  const el = await open();
  await choose(el, 2, 'archive-lib');
  await prefix(el, 2, '/mnt/archive');
  await choose(el, 3, 'photos-lib');
  button(el, 'Save')?.click();
  await vi.waitFor(() => expect(row(el, 3).textContent).toContain('Overlap in third row'));
  expect(row(el, 2).textContent).not.toContain('Overlap in third row');
  expect(row(el, 1).textContent).not.toContain('Overlap in third row');
});

it('discards edits and guards an unsaved draft', async () => {
  const el = await open();
  await choose(el, 1, 'photos-lib');
  const event = new Event('beforeunload', { cancelable: true });
  window.dispatchEvent(event);
  expect(event.defaultPrevented).toBe(true);
  button(el, 'Discard')?.click();
  await el.updateComplete;
  expect(row(el, 1).querySelector('input')?.value).toBe('');
  const clean = new Event('beforeunload', { cancelable: true });
  window.dispatchEvent(clean);
  expect(clean.defaultPrevented).toBe(false);
});

it('uses manual prefixes and null library ids in Limited mode without listing libraries', async () => {
  const el = await open(sampleImmichStatus({ mode: 'limited' }));
  expect(el.querySelector('opm-select')).toBeNull();
  expect(api.get).not.toHaveBeenCalledWith('/api/immich/libraries');
  await prefix(el, 1, '/mnt/photos');
  button(el, 'Save')?.click();
  await vi.waitFor(() =>
    expect(api.put).toHaveBeenCalledWith('/api/immich/mappings', {
      body: {
        mappings: [{ volume_id: 1, library_id: null, path_prefix: '/mnt/photos' }],
      },
    }),
  );
  expect(api.post).not.toHaveBeenCalled();
});

it('keeps saved mappings when sync fails and explicitly says they were saved', async () => {
  vi.mocked(api.put).mockResolvedValue({
    mappings: [
      {
        volume_id: 1,
        volume_name: 'photos',
        library_id: 'photos-lib',
        library_name: 'Photos',
        path_prefix: '/mnt/photos',
      },
    ],
  });
  vi.mocked(api.post).mockRejectedValue(
    new ApiError({ status: 502, code: 'immich_unreachable', message: 'raw' }),
  );
  const el = await open();
  await choose(el, 1, 'photos-lib');
  button(el, 'Save')?.click();
  await vi.waitFor(() =>
    expect(el.textContent).toContain('Mappings saved, but syncing could not start'),
  );
  expect(immichStatus.value?.mappings).toHaveLength(1);
});

it('ignores an already-unmapped sync conflict after a successful save', async () => {
  vi.mocked(api.put).mockResolvedValue({
    mappings: [
      {
        volume_id: 1,
        volume_name: 'photos',
        library_id: 'photos-lib',
        library_name: 'Photos',
        path_prefix: '/mnt/photos',
      },
    ],
  });
  vi.mocked(api.post).mockRejectedValue(
    new ApiError({ status: 409, code: 'immich_not_mapped', message: 'raw' }),
  );
  const el = await open();
  await choose(el, 1, 'photos-lib');
  button(el, 'Save')?.click();
  await vi.waitFor(() => expect(el.querySelector('opm-save-bar')).toBeNull());
  expect(el.querySelector('.alert-error')).toBeNull();
});

it('shows detect suggestions, Use and an empty-response explanation', async () => {
  vi.mocked(api.post).mockResolvedValue({
    suggestions: [{ prefix: '/detected', matched: 8, sampled: 10 }],
  });
  const el = await open();
  button(row(el, 1), 'Auto-detect')?.click();
  await vi.waitFor(() => expect(row(el, 1).textContent).toContain('Found 8 of 10 sampled files'));
  button(row(el, 1), 'Use')?.click();
  await el.updateComplete;
  expect(row(el, 1).querySelector('input')?.value).toBe('/detected');
  vi.mocked(api.post).mockResolvedValue({ suggestions: [] });
  button(row(el, 2), 'Auto-detect')?.click();
  await vi.waitFor(() =>
    expect(row(el, 2).textContent).toContain('knows none of the sampled files'),
  );
});

it('aborts and ignores a stale detect when the library changes or card leaves', async () => {
  let resolve!: (value: unknown) => void;
  vi.mocked(api.post).mockReturnValue(
    new Promise((res) => {
      resolve = res;
    }),
  );
  const el = await open();
  button(row(el, 1), 'Auto-detect')?.click();
  const signal = vi.mocked(api.post).mock.calls[0]?.[1]?.signal;
  await choose(el, 1, 'photos-lib');
  expect(signal?.aborted).toBe(true);
  resolve({ suggestions: [{ prefix: '/stale', matched: 1, sampled: 1 }] });
  await el.updateComplete;
  expect(el.textContent).not.toContain('/stale');
  button(row(el, 2), 'Auto-detect')?.click();
  const second = vi.mocked(api.post).mock.calls[1]?.[1]?.signal;
  el.remove();
  expect(second?.aborted).toBe(true);
  resolve({ suggestions: [] });
});

it('does not restart library loads from a detached component update', async () => {
  const el = await open();
  const calls = vi.mocked(api.get).mock.calls.length;
  el.remove();
  await el.updateComplete;
  expect(api.get).toHaveBeenCalledTimes(calls);
  document.body.append(el);
  await el.updateComplete;
  expect(api.get).toHaveBeenCalledTimes(calls + 1);
});

it('preserves unknown library options and renders every exclusion result from the server', async () => {
  const el = await open(
    sampleImmichStatus({
      mappings: [
        {
          volume_id: 1,
          volume_name: 'photos',
          library_id: 'missing',
          library_name: 'Old',
          path_prefix: '/mnt/photos',
        },
      ],
    }),
  );
  expect(row(el, 1).textContent).toContain('Unknown library');
  vi.mocked(api.post).mockResolvedValue({
    pattern: '**/server-trash/**',
    updated: [{ id: '1', name: 'Photos' }],
    already_excluded: [{ id: '2', name: 'Archive' }],
    failed: [{ id: '3', name: 'Other', message: 'Permission refused' }],
  });
  button(el, 'Keep the trash out of Immich')?.click();
  await vi.waitFor(() => expect(el.textContent).toContain('**/server-trash/**'));
  expect(el.textContent).toContain('Photos: updated');
  expect(el.textContent).toContain('Archive: already excluded');
  expect(el.textContent).toContain('Other: Permission refused');
});

it('retains drafts after a library reload failure and blocks Immich calls during reconnect', async () => {
  const el = await open();
  await choose(el, 1, 'photos-lib');
  vi.mocked(api.get).mockRejectedValue(new Error('Library load failed'));
  button(el, 'Reload libraries')?.click();
  await vi.waitFor(() => expect(el.textContent).toContain('Library load failed'));
  expect(row(el, 1).querySelector('input')?.value).toBe('/mnt/photos');
  applyImmichStatus(sampleImmichStatus({ needs_reconnect: true }));
  await el.updateComplete;
  await el.querySelector('opm-save-bar')?.updateComplete;
  expect(button(el, 'Save')?.disabled).toBe(true);
  expect(button(el, 'Reload libraries')?.disabled).toBe(true);
});
