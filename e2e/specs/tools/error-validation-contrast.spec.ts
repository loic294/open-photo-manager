import { expectNoSeriousA11yViolations } from '../../src/a11y';
import { expect, test } from '../../src/test';

test('invalid Rename pattern remains readable in both themes', async ({ app, page, shell }) => {
  await page.goto(`${app.url}/rename?src=photos/dupes/m1`);
  await page.getByText('Rename rules', { exact: false }).first().click();
  const pattern = page.getByRole('textbox', { name: 'Name pattern' });
  await pattern.fill('{UNKNOWN}');
  await expect(pattern).toHaveAttribute('aria-invalid', 'true');
  await expect(page.locator('apm-pattern-editor [role="alert"]')).toBeVisible();
  for (const theme of ['light', 'dark'] as const) {
    await shell.theme(theme);
    await expectNoSeriousA11yViolations(page, { include: ['apm-pattern-editor'] });
  }
});
