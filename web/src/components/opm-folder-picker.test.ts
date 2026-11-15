import { afterEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { folderEntry } from '@/test/folder-entry';
import { mount } from '@/test/helpers';
import './opm-folder-picker';

afterEach(() => vi.restoreAllMocks());
async function settle(el: Element) {
  for (let i = 0; i < 8; i++) {
    await Promise.resolve();
    await Promise.all(
      [el, ...el.querySelectorAll('*')].map((child) =>
        'updateComplete' in child ? child.updateComplete : undefined,
      ),
    );
  }
}

it('navigates indexed folders, keeps exact path bytes, and chooses the current scope', async () => {
  const post = vi.spyOn(api, 'post').mockResolvedValue({
    folders: [folderEntry({ volume_id: 3, path: 'e\u0301/day', name: 'day' })],
    counts: { in_folder: 0, in_subfolders: 0 },
  });
  const el = await mount('opm-folder-picker', {
    open: true,
    initial: { volume_id: 3, dir: 'e\u0301', recursive: true },
  });
  const picked = vi.fn();
  el.addEventListener('folder-picked', picked);
  await settle(el);
  el.querySelector('opm-folder-list')?.dispatchEvent(
    new CustomEvent('folder-open', {
      detail: { entry: folderEntry({ volume_id: 3, path: 'e\u0301/day' }) },
      bubbles: true,
    }),
  );
  await settle(el);
  expect(post).toHaveBeenLastCalledWith(
    '/api/browse/folders',
    expect.objectContaining({ body: expect.objectContaining({ dir: 'e\u0301/day' }) }),
  );
  [...el.querySelectorAll('button')]
    .find((button) => button.textContent?.trim() === 'Use this folder')
    ?.click();
  expect(picked.mock.calls[0]?.[0].detail).toEqual({
    volume_id: 3,
    dir: 'e\u0301/day',
    recursive: true,
  });
});

it('shows load errors and prevents selecting an unverified folder', async () => {
  vi.spyOn(api, 'post').mockRejectedValue(new Error('Folder is gone'));
  const el = await mount('opm-folder-picker', { open: true });
  await settle(el);
  expect(el.querySelector('[role="alert"]')?.textContent).toContain('Folder is gone');
  expect(
    [...el.querySelectorAll('button')].find(
      (button) => button.textContent?.trim() === 'Use all volumes',
    )?.disabled,
  ).toBe(true);
});
