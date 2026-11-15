import { afterEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { resetVolumesForTests } from '@/stores/volumes';
import { mount } from '@/test/helpers';
import { writesLabel } from '@/tools/settings/opm-settings-ism';
import { settingsSections } from '@/tools/settings/sections';

afterEach(() => {
  vi.restoreAllMocks();
  resetVolumesForTests();
});

const connection = {
  base_url: 'https://sync.example/',
  instance_id: 'instance-1',
  instance_name: 'Example sync',
};

const info = (directWrites: string) => ({
  instance_id: 'instance-1',
  instance_name: 'Example sync',
  version: '0.1.0',
  api_version: 1,
  token_kind: 'opm',
  actions: ['read', 'manage', 'trash'],
  roots: [
    {
      root_id: 'photos',
      name: 'Photos',
      immich_import_path: '/external/photos',
      available: true,
      direct_writes: directWrites,
      opm_prefix: '/volumes',
    },
  ],
});

const volume = { id: 7, name: 'photos', online: true, writable: true };

function serve(status: Record<string, unknown>) {
  return vi.spyOn(api, 'get').mockImplementation(async (path) => {
    if (path === '/api/volumes') return [volume];
    return status;
  });
}

it('connects and displays the actual root safety state without retaining the token', async () => {
  const get = serve({ connection: null, bound_volumes: 0, bindings: [], staged: [] });
  const put = vi.spyOn(api, 'put').mockResolvedValue(info('blocked'));
  const el = await mount('opm-settings-ism');
  await vi.waitFor(() => expect(el.querySelector('input[type="url"]')).not.toBeNull());
  const url = el.querySelector<HTMLInputElement>('input[type="url"]');
  const token = el.querySelector<HTMLInputElement>('input[type="password"]');
  if (!url || !token) throw new Error('Missing connection fields');
  url.value = 'https://sync.example';
  url.dispatchEvent(new Event('input'));
  token.value = 'fixture-token';
  token.dispatchEvent(new Event('input'));
  get.mockImplementation(async (path) =>
    path === '/api/volumes' ? [volume] : { connection, bound_volumes: 0, bindings: [], staged: [] },
  );
  el.querySelector('form')?.dispatchEvent(new Event('submit', { cancelable: true }));
  await vi.waitFor(() => expect(el.textContent).toContain('changes refused (blocked)'));
  expect(put).toHaveBeenCalledWith('/api/ism/connection', {
    body: { base_url: 'https://sync.example', token: 'fixture-token' },
  });
  expect(el.querySelector('input[type="password"]')).toBeNull();
  expect(el.textContent).toMatch(/never\s+changes files itself/);
  expect(settingsSections.find((section) => section.id === 'ism')?.tag).toBe('opm-settings-ism');
});

it('shows connection failures and prevents disconnecting a bound volume', async () => {
  serve({
    connection,
    bound_volumes: 1,
    bindings: [{ volume_id: 7, root_id: 'photos', root_name: 'Photos', root_prefix: 'photos' }],
    staged: [],
  });
  vi.spyOn(api, 'post').mockRejectedValue(new Error('Cannot reach ISM.'));
  const el = await mount('opm-settings-ism');
  await vi.waitFor(() => expect(el.textContent).toContain('1 bound volumes'));
  expect(el.textContent).toContain('Bound to Photos');
  const buttons = [...el.querySelectorAll('button')];
  const disconnect = buttons.find((button) => button.textContent?.includes('Disconnect'));
  expect(disconnect?.disabled).toBe(true);
  buttons.find((button) => button.textContent?.includes('Check connection'))?.click();
  await vi.waitFor(() =>
    expect(el.querySelector('[role="alert"]')?.textContent).toContain('Cannot reach ISM.'),
  );
});

it('stages a binding that applies at restart and shows why it waits', async () => {
  serve({ connection, bound_volumes: 0, bindings: [], staged: [] });
  vi.spyOn(api, 'post').mockResolvedValue(info('enabled'));
  const put = vi.spyOn(api, 'put').mockResolvedValue({
    connection,
    bound_volumes: 0,
    bindings: [],
    staged: [
      { volume_id: 7, root_id: 'photos', root_name: 'Photos', error: 'Pending Immich moves.' },
    ],
  });
  const el = await mount('opm-settings-ism');
  await vi.waitFor(() => expect(el.textContent).toContain('changes enabled'));
  expect(el.textContent).toContain('next time Open Photo Manager starts');
  const select = el.querySelector<HTMLSelectElement>('select');
  if (!select) throw new Error('Missing root select');
  select.value = 'photos';
  select.dispatchEvent(new Event('change'));
  await vi.waitFor(() => expect(el.textContent).toContain('Binds to Photos at restart'));
  expect(put).toHaveBeenCalledWith('/api/ism/bindings', {
    body: { volume_id: 7, root_id: 'photos' },
  });
  expect(el.textContent).toContain('Pending Immich moves.');
});

it('labels every direct_writes state, with the reason of a block', () => {
  const root = {
    root_id: 'photos',
    name: 'Photos',
    immich_import_path: '/external/photos' as string | null,
    opm_prefix: null,
    available: true,
    direct_writes: 'enabled',
    direct_writes_reason: null as string | null,
  };
  expect(writesLabel(root)).toBe('changes enabled');
  expect(writesLabel({ ...root, direct_writes: 'not_linked', immich_import_path: null })).toBe(
    'changes enabled (not linked to Immich)',
  );
  expect(
    writesLabel({ ...root, direct_writes: 'blocked', direct_writes_reason: 'Immich upgrade' }),
  ).toBe('changes refused (blocked: Immich upgrade)');
  expect(writesLabel({ ...root, direct_writes: 'disabled' })).toBe('changes refused (disabled)');
});
