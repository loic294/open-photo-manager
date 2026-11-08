import { signal } from '@preact/signals-core';
import { afterEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { FakeEventSource } from '@/test/fake-event-source';
import { mount } from '@/test/helpers';
import { sampleImmichStatus } from '@/test/immich-fixtures';
import '@/app/apm-app';

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
it('shows and clears the accessible Settings menu dot reactively', async () => {
  vi.stubGlobal('EventSource', FakeEventSource);
  vi.spyOn(api, 'get').mockImplementation(async (path) => {
    if (path === '/api/immich/status') return sampleImmichStatus();
    if (path === '/api/index/status') return { held_removals: [], active_task_id: null };
    return [];
  });
  const attention = signal(false);
  if (!customElements.get('apm-immich-test-page'))
    customElements.define('apm-immich-test-page', class extends HTMLElement {});
  const el = await mount('apm-app', {
    routes: [
      {
        path: '/settings',
        label: 'Settings',
        icon: 'settings',
        tag: 'apm-immich-test-page',
        menu: 'footer',
        load: async () => {},
        attention: () => attention.value,
      },
    ],
  });
  expect(el.querySelector('[aria-label="Needs attention"]')).toBeNull();
  attention.value = true;
  await el.updateComplete;
  expect(
    el.querySelector('[aria-label="Needs attention"]')?.classList.contains('status-warning'),
  ).toBe(true);
  attention.value = false;
  await el.updateComplete;
  expect(el.querySelector('[aria-label="Needs attention"]')).toBeNull();
});
