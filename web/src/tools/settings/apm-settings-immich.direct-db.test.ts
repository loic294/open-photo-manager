import { afterEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { applyImmichStatus, resetImmichForTests } from '@/stores/immich';
import { mount } from '@/test/helpers';
import { sampleImmichStatus, sampleOutboxPage } from '@/test/immich-fixtures';
import '@/tools/settings/apm-settings-immich';

afterEach(() => {
  resetImmichForTests();
  vi.restoreAllMocks();
});

it('shows actionable readiness, backup and scanner warnings without a database secret input', async () => {
  const status = sampleImmichStatus();
  status.direct_database = {
    ready: false,
    reason: 'Database unavailable. Check private networking and retry.',
    checked_at: null,
    warning:
      'Direct SQL is not officially supported. Keep a current backup; an active scan can race.',
  };
  applyImmichStatus(status);
  vi.spyOn(api, 'get').mockImplementation(async (path) =>
    path === '/api/immich/outbox'
      ? sampleOutboxPage({ items: [], total: 0 })
      : path === '/api/immich/libraries'
        ? []
        : status,
  );
  const el = await mount('apm-settings-immich');
  expect(el.textContent).toContain('Database unavailable');
  expect(el.textContent).toContain('Last executor check: not yet');
  expect(el.textContent).toContain('APM_IMMICH_DATABASE_URL_FILE');
  expect(el.textContent).toContain('sslmode=verify-full');
  expect(el.textContent).toContain('no file is moved');
  expect(el.textContent).toContain('Untracked files can continue');
  expect(el.textContent).toContain('durable outbox');
  expect(el.textContent).toContain('current backup');
  expect(el.textContent).toContain('scan can race');
  expect(el.querySelector('input[name*="database"], input[name*="postgres"]')).toBeNull();
});
