import { afterEach, describe, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { ApiError } from '@/api/errors';
import { mount } from '@/test/helpers';
import { sampleStatistics } from '@/test/statistics-fixtures';
import '@/tools/statistics/opm-statistics-page';

afterEach(() => vi.restoreAllMocks());

describe('Statistics page', () => {
  it('shows exact metrics, health and missing history alongside accessible charts', async () => {
    vi.spyOn(api, 'get').mockResolvedValue(sampleStatistics());
    const page = await mount('opm-statistics-page');
    await vi.waitFor(() => expect(page.textContent).toContain('Daily details'));
    expect(page.querySelectorAll('svg[role="img"]')).toHaveLength(3);
    expect(page.textContent).toContain('No observation');
    expect(page.textContent).toContain('Duplicate bytes trashed');
    expect(page.textContent).toContain('Metadata');
    expect(page.querySelector('h1')).toBeNull();
  });

  it('passes volume changes to the endpoint and aborts superseded requests', async () => {
    const get = vi.spyOn(api, 'get').mockResolvedValue(sampleStatistics());
    const page = await mount('opm-statistics-page');
    await vi.waitFor(() => expect(page.textContent).toContain('Daily details'));
    const previous = get.mock.calls[0]?.[1]?.signal;
    const select = page.querySelector<HTMLSelectElement>('[aria-label="Statistics volume"]');
    if (!select) throw new Error('Missing volume filter');
    select.value = '1';
    select.dispatchEvent(new Event('change'));
    await vi.waitFor(() =>
      expect(get).toHaveBeenLastCalledWith(
        '/api/statistics',
        expect.objectContaining({
          query: expect.objectContaining({ volume_id: 1 }),
        }),
      ),
    );
    expect(previous?.aborted).toBe(true);
    page.remove();
    expect(get.mock.calls.at(-1)?.[1]?.signal?.aborted).toBe(true);
  });

  it('reports server failure and allows retry without claiming an empty library', async () => {
    const get = vi.spyOn(api, 'get').mockRejectedValue(
      new ApiError({
        status: 500,
        code: 'internal',
        message: 'Statistics unavailable',
      }),
    );
    const page = await mount('opm-statistics-page');
    await vi.waitFor(() =>
      expect(page.querySelector('[role="alert"]')?.textContent).toContain('Statistics unavailable'),
    );
    expect(page.textContent).not.toContain('No inventory observations');
    get.mockResolvedValue(sampleStatistics());
    page.querySelector<HTMLButtonElement>('[role="alert"] button')?.click();
    await vi.waitFor(() => expect(page.textContent).toContain('Daily details'));
  });
});
