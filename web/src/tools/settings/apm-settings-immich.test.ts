import { afterEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { applyImmichStatus, loadImmichStatus, resetImmichForTests } from '@/stores/immich';
import { loadVolumes } from '@/stores/volumes';
import { mount } from '@/test/helpers';
import { sampleImmichStatus, sampleLibraries, sampleOutboxPage } from '@/test/immich-fixtures';
import { sampleSettings } from '@/test/settings-fixtures';
import { settingsSections } from '@/tools/settings/sections';
import '@/tools/settings/apm-settings-page';
import '@/tools/settings/apm-settings-immich';

afterEach(() => vi.restoreAllMocks());
it('shows initial loading and an actionable first-load failure with Retry', async () => {
  resetImmichForTests();
  const get = vi.spyOn(api, 'get').mockRejectedValue(new Error('Cannot load Immich'));
  const el = await mount('apm-settings-immich');
  expect(el.querySelector('.skeleton')).not.toBeNull();
  await loadImmichStatus();
  await el.updateComplete;
  expect(el.querySelector('[role="alert"]')?.textContent).toContain('Cannot load Immich');
  get.mockResolvedValue(
    sampleImmichStatus({ connected: false, mode: 'disconnected', connection: null }),
  );
  el.querySelector('button')?.click();
  await vi.waitFor(() => expect(el.querySelector('apm-immich-connection')).not.toBeNull());
});

it('shows only connection when disconnected, and all cards with reconnect gating when connected', async () => {
  vi.spyOn(api, 'get').mockImplementation(async (path) =>
    path === '/api/immich/libraries'
      ? { libraries: sampleLibraries, limited: false }
      : sampleOutboxPage(),
  );
  applyImmichStatus(
    sampleImmichStatus({ connected: false, mode: 'disconnected', connection: null }),
  );
  const el = await mount('apm-settings-immich');
  expect(el.querySelector('apm-immich-connection')).not.toBeNull();
  expect(el.querySelector('apm-immich-mappings')).toBeNull();
  applyImmichStatus(sampleImmichStatus({ needs_reconnect: true }));
  await el.updateComplete;
  for (const child of el.querySelectorAll(
    'apm-immich-connection,apm-immich-mappings,apm-immich-sync,apm-immich-outbox',
  )) {
    await (child as { updateComplete?: Promise<unknown> }).updateComplete;
  }
  expect(el.querySelector('apm-immich-mappings')).not.toBeNull();
  expect(el.querySelector('apm-immich-sync')).not.toBeNull();
  expect(el.querySelector('apm-immich-outbox')).not.toBeNull();
  expect(el.textContent).toContain('Connect again first');
});

it('registers the real lazy tab and keeps its mapping draft mounted across tab switches', async () => {
  vi.spyOn(api, 'get').mockImplementation(async (path) => {
    if (path === '/api/settings') return sampleSettings();
    if (path === '/api/volumes') return [{ id: 1, name: 'photos', online: true, writable: true }];
    if (path === '/api/immich/libraries') return { libraries: sampleLibraries, limited: false };
    return sampleOutboxPage();
  });
  applyImmichStatus(sampleImmichStatus());
  await loadVolumes();
  expect(settingsSections.find((section) => section.id === 'immich')?.tag).toBe(
    'apm-settings-immich',
  );
  window.history.replaceState(null, '', '/settings?section=immich');
  const page = await mount('apm-settings-page');
  await vi.waitFor(() => expect(page.querySelector('apm-settings-immich')).not.toBeNull());
  const section = page.querySelector('apm-settings-immich');
  await section?.updateComplete;
  const mappings = section?.querySelector('apm-immich-mappings');
  await mappings?.updateComplete;
  const input = mappings?.querySelector<HTMLInputElement>('input');
  if (!input) throw new Error('Missing mapping field');
  input.value = '/my-unsaved-prefix';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await mappings?.updateComplete;
  const general = [...page.querySelectorAll<HTMLButtonElement>('[role="tab"]')].find((tab) =>
    tab.textContent?.includes('General'),
  );
  general?.click();
  await page.updateComplete;
  expect(page.querySelector('apm-settings-immich')).toBe(section);
  expect(section?.closest('[role="tabpanel"]')?.hasAttribute('hidden')).toBe(true);
  const immich = [...page.querySelectorAll<HTMLButtonElement>('[role="tab"]')].find((tab) =>
    tab.textContent?.includes('Immich'),
  );
  immich?.click();
  await page.updateComplete;
  expect(mappings?.querySelector('input')?.value).toBe('/my-unsaved-prefix');
});
