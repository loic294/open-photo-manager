import { afterEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { pressConfirm } from '@/test/confirm';
import { mount } from '@/test/helpers';
import '@/tools/trash/opm-ism-trash';

afterEach(() => {
  vi.restoreAllMocks();
});

const binding = { volume_id: 7, root_id: 'photos', root_name: 'Photos', root_prefix: 'photos' };

function serve(canPurge: boolean) {
  vi.spyOn(api, 'get').mockImplementation(async (path) => {
    if (path === '/api/ism/status') {
      return { connection: null, bound_volumes: 1, bindings: [binding], staged: [] };
    }
    return {
      volume_id: 7,
      root_id: 'photos',
      root_name: 'Photos',
      can_purge: canPurge,
      items: [
        {
          trash_id: 't1',
          path: 'Trip/IMG_0001.jpg',
          is_folder: false,
          size: 2048,
          trashed_at: '2024-05-01T10:00:00Z',
          asset_count: 1,
        },
      ],
    };
  });
  return vi.spyOn(api, 'post').mockResolvedValue({ id: 41 });
}

const buttonNamed = (el: Element, label: string) =>
  [...el.querySelectorAll<HTMLButtonElement>('button')].find(
    (button) => button.textContent?.trim() === label,
  );

async function selectFirst(el: Element) {
  await vi.waitFor(() => expect(el.textContent).toContain('Trip/IMG_0001.jpg'));
  const box = el.querySelector<HTMLInputElement>('input[type="checkbox"]');
  if (!box) throw new Error('No checkbox');
  box.checked = true;
  box.dispatchEvent(new Event('change'));
  await (el as HTMLElement & { updateComplete: Promise<unknown> }).updateComplete;
}

it('restores selected items through the sync manager and hides purge without the grant', async () => {
  const post = serve(false);
  const el = await mount('opm-ism-trash');
  await selectFirst(el);
  expect(buttonNamed(el, 'Delete for good…')).toBeUndefined();
  buttonNamed(el, 'Restore')?.click();
  await vi.waitFor(() =>
    expect(post).toHaveBeenCalledWith('/api/trash/ism/restore', {
      body: { volume_id: 7, trash_ids: ['t1'] },
    }),
  );
});

it('purges only after an explicit confirmation', async () => {
  const post = serve(true);
  const el = await mount('opm-ism-trash');
  await selectFirst(el);
  buttonNamed(el, 'Delete for good…')?.click();
  await pressConfirm('Cancel');
  await Promise.resolve();
  expect(post).not.toHaveBeenCalled();

  buttonNamed(el, 'Delete for good…')?.click();
  await pressConfirm('Delete 1 item for good');
  await vi.waitFor(() =>
    expect(post).toHaveBeenCalledWith('/api/trash/ism/purge', {
      body: { volume_id: 7, trash_ids: ['t1'], confirm: true },
    }),
  );
});

it('renders nothing when no volume is bound', async () => {
  vi.spyOn(api, 'get').mockResolvedValue({
    connection: null,
    bound_volumes: 0,
    bindings: [],
    staged: [],
  });
  const el = await mount('opm-ism-trash');
  await Promise.resolve();
  expect(el.querySelector('section')).toBeNull();
});
