import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { ApiError } from '@/api/errors';
import { applyImmichStatus } from '@/stores/immich';
import { pressConfirm, shownConfirm } from '@/test/confirm';
import { mount } from '@/test/helpers';
import { sampleImmichStatus, sampleOutboxPage } from '@/test/immich-fixtures';
import '@/tools/settings/immich/apm-immich-outbox';

const button = (el: Element, text: string) =>
  [...el.querySelectorAll('button')].find((item) => item.textContent?.trim() === text);
async function open() {
  const el = await mount('apm-immich-outbox');
  await vi.waitFor(() => expect(el.textContent).toContain('photos/a.jpg'));
  return el;
}
async function select(el: Element) {
  const checkbox = el.querySelector<HTMLInputElement>('[aria-label="Select change 1"]');
  if (!checkbox) throw new Error('Missing checkbox');
  checkbox.checked = true;
  checkbox.dispatchEvent(new Event('change'));
  await (el as HTMLElementTagNameMap['apm-immich-outbox']).updateComplete;
}

beforeEach(() => {
  applyImmichStatus(sampleImmichStatus());
  vi.spyOn(api, 'get').mockImplementation(async (path) =>
    path === '/api/immich/status' ? sampleImmichStatus() : sampleOutboxPage(),
  );
  vi.spyOn(api, 'post').mockResolvedValue({ retried: 1, dismissed: 1 });
});
afterEach(() => vi.restoreAllMocks());

it('shows caught up, attention and pending/limited summaries without hiding failure reasons', async () => {
  const el = await open();
  expect(el.textContent).toContain('All caught up');
  applyImmichStatus(
    sampleImmichStatus({
      needs_attention: 2,
      outbox_pending: 3,
      outbox: {
        pending: 3,
        attention: 2,
        waiting_for_immich: 1,
        oldest_pending_at: 1700000000,
        last_run_at: 1700000100,
        last_error: 'immich_unavailable',
        scans_on_demand: false,
      },
    }),
  );
  await el.updateComplete;
  expect(el.textContent).toContain('2 changes need your decision');
  expect(el.textContent).toContain('3 changes will be sent');
  expect(el.textContent).toContain('cannot trigger a library scan');
  expect(el.textContent).toContain('Rescan its library');
  expect(el.querySelector('[title="photos/a.jpg"]')).not.toBeNull();
});

it('loads more with exact paging and only loads waiting changes on opening the collapse', async () => {
  vi.mocked(api.get).mockResolvedValue(sampleOutboxPage({ total: 51 }));
  const el = await open();
  expect(api.get).toHaveBeenCalledWith(
    '/api/immich/outbox',
    expect.objectContaining({ query: { state: 'attention', offset: 0, limit: 50 } }),
  );
  button(el, 'Load more')?.click();
  await vi.waitFor(() =>
    expect(api.get).toHaveBeenCalledWith(
      '/api/immich/outbox',
      expect.objectContaining({ query: { state: 'attention', offset: 2, limit: 50 } }),
    ),
  );
  const details = el.querySelector('details');
  if (!details) throw new Error('Missing collapse');
  details.open = true;
  details.dispatchEvent(new Event('toggle'));
  await vi.waitFor(() =>
    expect(api.get).toHaveBeenCalledWith(
      '/api/immich/outbox',
      expect.objectContaining({ query: { state: 'pending', offset: 0, limit: 50 } }),
    ),
  );
});

it('retries selected and all with exact ids, and reloads summary/list', async () => {
  const el = await open();
  await select(el);
  button(el, 'Retry selected')?.click();
  await vi.waitFor(() =>
    expect(api.post).toHaveBeenCalledWith('/api/immich/outbox/retry', { body: { ids: [1] } }),
  );
  await vi.waitFor(() =>
    expect(api.get).toHaveBeenCalledWith('/api/immich/status', expect.anything()),
  );
  await vi.waitFor(() => expect(button(el, 'Retry all')?.disabled).toBe(false));
  button(el, 'Retry all')?.click();
  await vi.waitFor(() =>
    expect(api.post).toHaveBeenCalledWith('/api/immich/outbox/retry', { body: { ids: null } }),
  );
});

it('confirms dismiss consequences and sends selected ids', async () => {
  const el = await open();
  await select(el);
  button(el, 'Dismiss selected')?.click();
  const confirmation = await shownConfirm();
  expect(confirmation.message).toContain('old entries stay in Immich until removed by hand');
  expect(api.post).not.toHaveBeenCalled();
  await pressConfirm('Dismiss selected');
  await vi.waitFor(() =>
    expect(api.post).toHaveBeenCalledWith('/api/immich/outbox/dismiss', { body: { ids: [1] } }),
  );
});

it('keeps selection on failure, reloads disconnected status, and supports a header selection', async () => {
  const el = await open();
  const checkbox = el.querySelector<HTMLInputElement>(
    '[aria-label="Select loaded attention changes"]',
  );
  if (!checkbox) throw new Error('Missing header');
  checkbox.checked = true;
  checkbox.dispatchEvent(new Event('change'));
  await el.updateComplete;
  vi.mocked(api.post).mockRejectedValue(
    new ApiError({ status: 409, code: 'immich_not_connected', message: 'raw' }),
  );
  button(el, 'Retry selected')?.click();
  await vi.waitFor(() =>
    expect(el.querySelector('.alert-error')?.textContent).toContain('Connect to Immich first'),
  );
  expect(el.querySelector<HTMLInputElement>('[aria-label="Select change 1"]')?.checked).toBe(true);
  expect(el.querySelector<HTMLInputElement>('[aria-label="Select change 2"]')?.checked).toBe(true);
  expect(api.get).toHaveBeenCalledWith('/api/immich/status', expect.anything());
});

it('does not restart list loads after removal and reloads on reconnection', async () => {
  const el = await open();
  const calls = vi.mocked(api.get).mock.calls.length;
  el.remove();
  await el.updateComplete;
  expect(api.get).toHaveBeenCalledTimes(calls);
  document.body.append(el);
  await el.updateComplete;
  expect(api.get).toHaveBeenCalledTimes(calls + 1);
});
