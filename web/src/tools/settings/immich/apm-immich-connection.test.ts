import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { applyImmichStatus, immichStatus } from '@/stores/immich';
import { pressConfirm, shownConfirm } from '@/test/confirm';
import { mount } from '@/test/helpers';
import { sampleImmichStatus } from '@/test/immich-fixtures';
import '@/tools/settings/immich/apm-immich-connection';

const button = (el: Element, text: string) =>
  [...el.querySelectorAll('button')].find((item) => item.textContent?.trim() === text);
beforeEach(() => {
  applyImmichStatus(sampleImmichStatus());
  vi.spyOn(api, 'get').mockResolvedValue(sampleImmichStatus());
});
afterEach(() => {
  document.querySelector('apm-confirm-dialog')?.remove();
  vi.restoreAllMocks();
});

it('shows account, version, safe external link and Full mode', async () => {
  applyImmichStatus(sampleImmichStatus({ version_warning: 'Newer than supported' }));
  const el = await mount('apm-immich-connection');
  expect(el.textContent).toContain('Administrator');
  expect(el.textContent).toContain('Test admin');
  expect(el.textContent).toContain('Newer than supported');
  expect(el.textContent).toContain('Libraries are listed');
  expect(el.querySelector('a')?.rel).toBe('noopener noreferrer');
  expect(el.querySelector('a')?.target).toBe('_blank');
  expect(button(el, 'Disconnect')?.classList.contains('btn-error')).toBe(false);
});

it('shows regular limited accounts, reachability recovery and explicit connection checks', async () => {
  const status = sampleImmichStatus({ mode: 'limited', reachable: false });
  if (!status.connection) throw new Error('Missing connection');
  status.connection = { ...status.connection, is_admin: false, last_error: 'immich_unreachable' };
  applyImmichStatus(status);
  const el = await mount('apm-immich-connection');
  expect(el.textContent).toContain('Regular account');
  expect(el.textContent).toContain('own library scan schedule');
  expect(el.textContent).toContain('resume by themselves');
  button(el, 'Check connection')?.click();
  await vi.waitFor(() =>
    expect(api.get).toHaveBeenCalledWith('/api/immich/status', {
      query: { check: true },
      signal: undefined,
    }),
  );
});

it('leads with reconnect warning, pre-filled form and disables checks', async () => {
  applyImmichStatus(sampleImmichStatus({ needs_reconnect: true }));
  const el = await mount('apm-immich-connection');
  await el.querySelector('apm-immich-connect-form')?.updateComplete;
  expect(el.textContent).toContain('no longer accepts the stored key');
  expect(el.querySelector<HTMLInputElement>('[name="password"]')?.value).toBe('');
  expect(button(el, 'Check connection')?.disabled).toBe(true);
});

it.each([true, false])(
  'keeps a persistent disconnect result (revoked=%s) and confirms forgotten count',
  async (revoked) => {
    applyImmichStatus(sampleImmichStatus({ outbox_pending: 2, needs_attention: 3 }));
    vi.spyOn(api, 'delete').mockResolvedValue({
      revoked,
      message: revoked ? null : 'Server offline',
    });
    const el = await mount('apm-immich-connection');
    button(el, 'Disconnect')?.click();
    const dialog = await shownConfirm();
    expect(dialog.message).toContain('5 changes');
    expect(dialog.message).toContain('mappings are kept');
    await pressConfirm('Disconnect');
    await vi.waitFor(() => expect(immichStatus.value?.connected).toBe(false));
    expect(el.textContent).toContain(
      revoked ? 'API key was revoked' : 'Remove the key named APM test',
    );
    expect(el.querySelector(revoked ? '.alert-success' : '.alert-warning')).not.toBeNull();
    button(el, 'Dismiss')?.click();
    await el.updateComplete;
    expect(el.querySelector('[aria-label="Dismiss disconnect result"]')).toBeNull();
  },
);
