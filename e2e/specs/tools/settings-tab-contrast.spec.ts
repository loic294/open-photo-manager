import { expectNoSeriousA11yViolations } from '../../src/a11y';
import { expect, test } from '../../src/test';

for (const theme of ['light', 'dark'] as const) {
  test(`Settings inactive tabs retain readable contrast in ${theme}`, async ({
    app,
    page,
  }, info) => {
    await page.goto(`${app.url}/settings`);
    await page.getByLabel('Full rescan interval', { exact: true }).fill('32');
    await page.getByRole('button', { name: 'Save', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Save', exact: true })).toHaveCount(0);
    expect((await app.api.settings()).index.rescan_interval_minutes).toBe(32);
    await page.locator('html').evaluate((element, value) => {
      element.setAttribute('data-theme', value);
    }, theme);

    const inactive = page.getByRole('tab', { name: 'Immich', exact: true });
    await expect(inactive).toHaveAttribute('aria-selected', 'false');
    await expectNoSeriousA11yViolations(page);
    await inactive.hover();
    await expectNoSeriousA11yViolations(page);
    await inactive.focus();
    await expectNoSeriousA11yViolations(page);
    await expect(inactive).toHaveAttribute('aria-selected', 'false');

    await info.attach('inactive-tab-style', {
      body: JSON.stringify(
        await inactive.evaluate((element) => {
          const style = getComputedStyle(element);
          return {
            theme: document.documentElement.dataset.theme,
            color: style.color,
            opacity: style.opacity,
            background: getComputedStyle(document.documentElement).backgroundColor,
          };
        }),
      ),
      contentType: 'application/json',
    });
  });
}
