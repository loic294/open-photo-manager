import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { events } from '@/api/sse';
import type { OpmPane } from '@/components/opm-pane';
import '@/components/opm-pane';
import { setLocation, syncFromLocation } from '@/stores/nav';
import { resetSelectionForTests } from '@/stores/selection';
import { clearToasts } from '@/stores/toasts';
import { loadVolumes, volumesStatus } from '@/stores/volumes';
import { stubBrowseServer } from '@/test/browse-server';
import { mount } from '@/test/helpers';

// The volumes store is a module singleton that never goes back to `idle`. This file exists so
// that its one scenario starts from a store nobody has loaded: every test file gets its own.

beforeEach(() => {
  window.history.replaceState(null, '', '/');
  syncFromLocation();
});

afterEach(() => {
  events.stop();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  clearToasts();
  resetSelectionForTests();
});

const text = (element: Element | null) => element?.textContent?.replace(/\s+/g, ' ').trim();
const alertText = (pane: OpmPane) => text(pane.querySelector('[role="alert"]'));
const rows = (pane: OpmPane) => [...pane.querySelectorAll('opm-folder-list a[data-name]')];

describe('opm-pane before the volumes are known', () => {
  it('waits for them, reports a failure with a retry, and lists once they are there', async () => {
    const server = stubBrowseServer({ volumesFailure: 'the database is locked' });
    setLocation('source', { volume: 'photos', dir: 'trip' });

    const pane = await mount('opm-pane', { pane: 'source', heading: 'Source' });

    // Never loaded: skeleton rows and placeholders, no request, the path still shown.
    expect(volumesStatus.value).toBe('idle');
    expect(pane.browseRequest).toBeNull();
    expect(pane.querySelector('opm-folder-list')?.hasAttribute('loading')).toBe(true);
    expect(pane.querySelector('opm-folder-list [role="status"]')).not.toBeNull();
    expect(pane.querySelector('[role="alert"]')).toBeNull();
    expect([...pane.querySelectorAll('[data-crumbs] a')].map(text)).toEqual([
      'Volumes',
      'photos',
      'trip',
    ]);
    expect(server.calls.folders).toEqual([]);
    expect(server.calls.items).toEqual([]);

    // The first load is under way.
    server.hold('volumes');
    const loading = loadVolumes();
    await vi.waitFor(() => expect(volumesStatus.value).toBe('loading'));
    await pane.updateComplete;
    expect(pane.querySelector('[role="alert"]')).toBeNull();
    expect(pane.querySelector('opm-folder-list')?.hasAttribute('loading')).toBe(true);

    // It fails: say so, and offer to try again. Nothing is requested for an unknown volume.
    server.release('volumes');
    await loading;
    await vi.waitFor(() => expect(volumesStatus.value).toBe('error'));
    await pane.updateComplete;
    expect(alertText(pane)).toContain('Could not load the volumes: the database is locked');
    expect(pane.querySelector('opm-folder-list')).toBeNull();
    expect(pane.querySelector('opm-file-grid')).toBeNull();
    expect(server.calls.folders).toEqual([]);

    // The retry succeeds: the alert goes and the folder is listed.
    server.state.volumesFailure = null;
    server.state.folders = () => ({
      folders: [
        {
          name: 'raw',
          path: 'trip/raw',
          volume_id: 1,
          count: 2,
          total: 2,
          matches: null,
          preview: null,
          overlay: { matched: false, planned: false, will_delete: false },
        },
      ],
      counts: { in_folder: 4, in_subfolders: 2 },
    });
    pane.querySelector<HTMLButtonElement>('[role="alert"] button')?.click();
    await vi.waitFor(() =>
      expect(rows(pane).map((row) => row.getAttribute('data-name'))).toEqual(['raw']),
    );

    expect(volumesStatus.value).toBe('ready');
    expect(pane.querySelector('[role="alert"]')).toBeNull();
    expect(pane.browseRequest).toMatchObject({ volume_id: 1, dir: 'trip' });
    expect(server.calls.folders).toHaveLength(1);
  });
});
