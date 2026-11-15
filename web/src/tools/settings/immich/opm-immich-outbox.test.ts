import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { ApiError } from '@/api/errors';
import type { OutboxPage } from '@/api/generated/OutboxPage';
import { applyImmichStatus } from '@/stores/immich';
import { pressConfirm, shownConfirm } from '@/test/confirm';
import { mount } from '@/test/helpers';
import { sampleImmichStatus, sampleOutboxPage } from '@/test/immich-fixtures';
import '@/tools/settings/immich/opm-immich-outbox';

const button = (el: Element, text: string) =>
  [...el.querySelectorAll('button')].find((item) => item.textContent?.trim() === text);
async function open() {
  const el = await mount('opm-immich-outbox');
  await vi.waitFor(() => expect(el.textContent).toContain('photos/a.jpg'));
  return el;
}
async function select(el: Element) {
  const checkbox = el.querySelector<HTMLInputElement>('[aria-label="Select change 1"]');
  if (!checkbox) throw new Error('Missing checkbox');
  checkbox.checked = true;
  checkbox.dispatchEvent(new Event('change'));
  await (el as HTMLElementTagNameMap['opm-immich-outbox']).updateComplete;
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
  expect(el.querySelector('[data-tooltip="photos/a.jpg"]')).not.toBeNull();
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

it('coalesces status updates during a list load into one non-aborting trailing refresh', async () => {
  let resolveFirst!: (page: OutboxPage) => void;
  const first = new Promise<OutboxPage>((resolve) => {
    resolveFirst = resolve;
  });
  vi.mocked(api.get)
    .mockReturnValueOnce(first)
    .mockResolvedValue(sampleOutboxPage({ items: [], total: 0 }));
  const el = await mount('opm-immich-outbox');
  const signal = vi.mocked(api.get).mock.calls[0]?.[1]?.signal;
  expect(signal).toBeDefined();
  for (const needs_attention of [1, 2]) {
    applyImmichStatus(sampleImmichStatus({ needs_attention }));
    await el.updateComplete;
  }
  expect(signal?.aborted).toBe(false);
  expect(api.get).toHaveBeenCalledTimes(1);
  resolveFirst(sampleOutboxPage());
  await vi.waitFor(() => {
    expect(api.get).toHaveBeenCalledTimes(2);
    expect(el.textContent).toContain('No changes needing attention.');
  });
  expect(signal?.aborted).toBe(false);
  expect(el.querySelector('[data-tooltip="photos/a.jpg"]')).toBeNull();
});

it('discards a queued status refresh when removed during a list load', async () => {
  let resolveFirst!: (page: OutboxPage) => void;
  const first = new Promise<OutboxPage>((resolve) => {
    resolveFirst = resolve;
  });
  vi.mocked(api.get).mockReturnValueOnce(first);
  const el = await mount('opm-immich-outbox');
  const signal = vi.mocked(api.get).mock.calls[0]?.[1]?.signal;
  applyImmichStatus(sampleImmichStatus({ needs_attention: 1 }));
  await el.updateComplete;
  el.remove();
  expect(signal?.aborted).toBe(true);
  resolveFirst(sampleOutboxPage());
  await first;
  await el.updateComplete;
  expect(api.get).toHaveBeenCalledTimes(1);
});

it('waits for the pending list too before running a queued status refresh', async () => {
  const el = await open();
  let resolvePending!: (page: OutboxPage) => void;
  const pending = new Promise<OutboxPage>((resolve) => {
    resolvePending = resolve;
  });
  vi.mocked(api.get).mockClear().mockReturnValueOnce(pending);
  const details = el.querySelector('details');
  if (!details) throw new Error('Missing collapse');
  details.open = true;
  details.dispatchEvent(new Event('toggle'));
  await el.updateComplete;
  const signal = vi.mocked(api.get).mock.calls[0]?.[1]?.signal;
  for (const needs_attention of [1, 2]) {
    applyImmichStatus(sampleImmichStatus({ needs_attention }));
    await el.updateComplete;
  }
  expect(signal?.aborted).toBe(false);
  expect(api.get).toHaveBeenCalledTimes(1);
  resolvePending(sampleOutboxPage({ items: [], total: 0 }));
  await vi.waitFor(() => {
    expect(api.get).toHaveBeenCalledTimes(3);
    expect(el.textContent).not.toContain('Loading changes');
  });
  expect(signal?.aborted).toBe(false);
  expect(
    vi
      .mocked(api.get)
      .mock.calls.slice(1)
      .map(([, options]) => options?.query?.state),
  ).toEqual(['attention', 'pending']);
});
