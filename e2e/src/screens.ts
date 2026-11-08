import { mkdir } from 'node:fs/promises';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { root } from './paths';

export async function captureScreens(page: Page, name: string) {
  if (!/^[a-zA-Z0-9_-]+$/.test(name)) throw new Error('Screenshot name must be a slug');
  const directory = join(root, 'e2e/test-results/screens');
  await mkdir(directory, { recursive: true });
  const original = await page.locator('html').getAttribute('data-theme');
  const paths: string[] = [];
  for (const theme of ['light', 'dark']) {
    await page
      .locator('html')
      .evaluate((element, value) => element.setAttribute('data-theme', value), theme);
    const path = join(directory, `${name}-${theme}.png`);
    await page.screenshot({ path, fullPage: false });
    paths.push(path);
  }
  await page
    .locator('html')
    .evaluate(
      (element, value) =>
        value === null
          ? element.removeAttribute('data-theme')
          : element.setAttribute('data-theme', value),
      original,
    );
  return paths;
}
