import { expectNoSeriousA11yViolations } from '../src/a11y';
import { expect, test } from '../src/test';

test('real shell and volume navigation have no serious accessibility violations', async ({
  sharedApp,
  page,
  source,
}) => {
  await page.goto(sharedApp.url);
  await expect(source.folder('photos')).toBeVisible();
  await expectNoSeriousA11yViolations(page);
});
