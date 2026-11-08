import type { FormatPreviewResponse } from '../../../web/src/api/generated/FormatPreviewResponse';
import { expect, test } from '../../src/test';

function isPreviewFor(pattern: string) {
  return (response: import('@playwright/test').Response) => {
    const request = response.request();
    if (request.method() !== 'POST' || new URL(response.url()).pathname !== '/api/format/preview')
      return false;
    try {
      return (JSON.parse(request.postData() ?? '{}') as { pattern?: string }).pattern === pattern;
    } catch {
      return false;
    }
  };
}

test('invalid format patterns show API issues and disable execution', async ({
  app,
  page,
  formatPage,
}) => {
  const url = new URL('/format', app.url);
  url.searchParams.set('src', 'photos/dupes/m1');
  const initialPreview = page.waitForResponse(isPreviewFor('{YYYY}/{mm}'));
  const initialPlan = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/plans',
  );
  await page.goto(url.href);
  await initialPreview;
  await initialPlan;

  for (const pattern of ['{UNKNOWN}', '../outside', '/absolute', '{YYYY']) {
    const previewResponse = page.waitForResponse(isPreviewFor(pattern));
    await formatPage.pattern().fill(pattern);
    const preview = await previewResponse;
    expect(preview.ok()).toBe(true);
    const body = (await preview.json()) as FormatPreviewResponse;
    expect(body.error).not.toBeNull();
    await expect(formatPage.pattern()).toHaveAttribute('aria-invalid', 'true');
    await expect(page.getByRole('alert').last()).toBeVisible();
    await expect(formatPage.actionButton()).toBeDisabled();
  }
});
