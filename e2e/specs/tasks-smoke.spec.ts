import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { writeFile } from '../src/disk';
import { browseUrl, fixture } from '../src/scenarios';
import { expect, test } from '../src/test';

test('menu re-index reports progress while panes remain interactive', async ({
  app,
  page,
  source,
  shell,
  manifest,
}) => {
  test.setTimeout(120_000);
  const file = fixture(manifest, 'exif.full_nikon');
  await page.goto(browseUrl(app.url, `${file.volume}/${file.dir}`));
  await expect(source.card(file.name).element).toBeVisible();
  const bytes = await readFile(join(app.volumesDir, file.volume, file.path));
  for (let i = 0; i < 20; i++)
    await writeFile(app, file.volume, `${file.dir}/e2e-task-${i}.jpg`, bytes);
  await shell.reindex();
  await expect(shell.progress()).toBeVisible();
  await source.card(file.name).select();
  await expect(source.card(file.name).element).toHaveAttribute('aria-selected', 'true');
  await app.waitForIdle();
  await expect(source.card('e2e-task-0.jpg').element).toBeVisible();
  expect(
    (await app.api.listing(file.volume, file.dir)).items.some(
      (item) => item.name === 'e2e-task-0.jpg',
    ),
  ).toBe(true);
});
