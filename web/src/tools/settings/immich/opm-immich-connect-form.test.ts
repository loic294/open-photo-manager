import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { ApiError } from '@/api/errors';
import { applyImmichStatus, immichStatus } from '@/stores/immich';
import { mount } from '@/test/helpers';
import { sampleImmichStatus } from '@/test/immich-fixtures';
import '@/tools/settings/immich/opm-immich-connect-form';

type Form = HTMLElementTagNameMap['opm-immich-connect-form'];
async function type(el: Form, field: string, value: string) {
  const input = el.querySelector<HTMLInputElement>(`[name="${field}"]`);
  if (!input) throw new Error(`Missing ${field}`);
  input.value = value;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await el.updateComplete;
}
async function fill(el: Form) {
  await type(el, 'server_url', 'http://localhost:12345');
  await type(el, 'email', 'admin@example.test');
  await type(el, 'password', 'test-password-only');
}
const submit = (el: Form) =>
  el.querySelector('form')?.dispatchEvent(new Event('submit', { cancelable: true, bubbles: true }));

beforeEach(() => {
  applyImmichStatus(
    sampleImmichStatus({ connected: false, mode: 'disconnected', connection: null }),
  );
  vi.spyOn(api, 'get').mockResolvedValue(sampleImmichStatus());
});
afterEach(() => vi.restoreAllMocks());

it('sends the exact request and clears the password from state, DOM and storage after success', async () => {
  const post = vi.spyOn(api, 'post').mockResolvedValue(sampleImmichStatus().connection);
  const log = vi.spyOn(console, 'log');
  const warn = vi.spyOn(console, 'warn');
  const error = vi.spyOn(console, 'error');
  const el = await mount('opm-immich-connect-form');
  await fill(el);
  const toggle = el.querySelector<HTMLInputElement>('.toggle');
  if (!toggle) throw new Error('Missing toggle');
  toggle.checked = true;
  toggle.dispatchEvent(new Event('change'));
  submit(el);
  await vi.waitFor(() => expect(post).toHaveBeenCalledOnce());
  await el.updateComplete;
  expect(post).toHaveBeenCalledWith('/api/immich/connect', {
    body: {
      server_url: 'http://localhost:12345',
      email: 'admin@example.test',
      password: 'test-password-only',
      allow_insecure_tls: true,
    },
  });
  expect(el.querySelector<HTMLInputElement>('[name="password"]')?.value).toBe('');
  expect(el.outerHTML).not.toContain('test-password-only');
  expect(JSON.stringify(immichStatus.value)).not.toContain('test-password-only');
  expect(JSON.stringify(localStorage)).not.toContain('test-password-only');
  expect(log).not.toHaveBeenCalled();
  expect(warn).not.toHaveBeenCalled();
  expect(error).not.toHaveBeenCalled();
});

it('keeps one pending request, disables the whole form and ignores a duplicate submit', async () => {
  let resolve!: (value: unknown) => void;
  const post = vi.spyOn(api, 'post').mockReturnValue(
    new Promise((res) => {
      resolve = res;
    }),
  );
  const el = await mount('opm-immich-connect-form');
  await fill(el);
  submit(el);
  submit(el);
  await el.updateComplete;
  expect(post).toHaveBeenCalledOnce();
  expect(el.querySelector('fieldset')?.disabled).toBe(true);
  expect(el.textContent).toContain('Connecting…');
  resolve(sampleImmichStatus().connection);
  await vi.waitFor(() =>
    expect(el.querySelector<HTMLInputElement>('[name="password"]')?.value).toBe(''),
  );
});

it('does not reuse database readiness from the previous connection while refreshing status', async () => {
  const previous = sampleImmichStatus();
  previous.direct_database.ready = true;
  previous.direct_database.checked_at = 123;
  applyImmichStatus(previous);
  vi.mocked(api.get).mockReturnValue(new Promise(() => {}));
  vi.spyOn(api, 'post').mockResolvedValue(sampleImmichStatus().connection);
  const el = await mount('opm-immich-connect-form');
  await fill(el);
  submit(el);
  await vi.waitFor(() => expect(immichStatus.value?.direct_database.ready).toBe(false));
  expect(immichStatus.value?.direct_database.checked_at).toBeNull();
  expect(immichStatus.value?.direct_database.reason).toContain('checked again');
  expect(immichStatus.value?.direct_database.warning).toBe(previous.direct_database.warning);
});

it('uses a native submit form for Enter and pre-fills reconnect without a password', async () => {
  const el = await mount('opm-immich-connect-form', {
    connection: sampleImmichStatus().connection,
  });
  expect(el.querySelector<HTMLInputElement>('[name="server_url"]')?.value).toBe(
    'http://127.0.0.1:12283',
  );
  expect(el.querySelector<HTMLInputElement>('[name="email"]')?.value).toBe('admin@example.test');
  expect(el.querySelector<HTMLInputElement>('[name="password"]')?.value).toBe('');
  expect(el.querySelector('button')?.type).toBe('submit');
  expect(el.querySelector('form')?.hasAttribute('novalidate')).toBe(false);
});

it('places all 422 credential errors at the named fields', async () => {
  vi.spyOn(api, 'post').mockRejectedValue(
    new ApiError({
      status: 422,
      code: 'validation_failed',
      message: 'Refused',
      fields: ['server_url', 'email', 'password'].map((field) => ({
        field,
        message: `Bad ${field}`,
      })),
    }),
  );
  const el = await mount('opm-immich-connect-form');
  await fill(el);
  submit(el);
  await vi.waitFor(() => expect(el.textContent).toContain('Bad password'));
  expect(el.querySelectorAll('[aria-invalid="true"]')).toHaveLength(3);
  expect(el.querySelector('[name="password"]')?.getAttribute('aria-label')).toBe('Password');
  expect(
    el.querySelector('#immich-connect-password-help')?.classList.contains('text-base-content'),
  ).toBe(true);
  expect(el.textContent).toContain('Bad server_url');
  expect(el.textContent).toContain('Bad email');
});

it('shows unreachable failures without losing typed values and clears on removal', async () => {
  vi.spyOn(api, 'post').mockRejectedValue(
    new ApiError({ status: 502, code: 'immich_unreachable', message: 'raw' }),
  );
  const el = await mount('opm-immich-connect-form');
  await fill(el);
  submit(el);
  await vi.waitFor(() =>
    expect(el.querySelector('[role="alert"]')?.textContent).toContain('could not be reached'),
  );
  expect(el.querySelector<HTMLInputElement>('[name="password"]')?.value).toBe('test-password-only');
  expect(el.querySelector<HTMLInputElement>('[name="server_url"]')?.value).toBe(
    'http://localhost:12345',
  );
  el.remove();
  expect(el.querySelector<HTMLInputElement>('[name="password"]')?.value).toBe('');
});
