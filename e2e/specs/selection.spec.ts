import { expectNoSeriousA11yViolations } from '../src/a11y';
import { browseUrl, fixture } from '../src/scenarios';
import { expect, test } from '../src/test';

test.beforeEach(async ({ page, sharedApp, manifest }) => {
  const file = fixture(manifest, 'sidecar.exact');
  await page.goto(browseUrl(sharedApp.url, `${file.volume}/${file.dir}`));
});

test('checkbox, modifier click and shift range select in either pane @smoke', async ({
  source,
  comparison,
  viewer,
}) => {
  await expect.poll(() => source.names()).not.toEqual([]);
  const names = await source.names();
  for (const pane of [source, comparison]) {
    const first = pane.card(names[0]);
    await first.expectCheckboxTopRight();
    await first.select();
    await expect(first.element).toHaveAttribute('aria-selected', 'true');
    await first.expectBorder('primary');
    await pane.card(names[1]).click(['ControlOrMeta']);
    await pane.card(names[3]).click(['Shift']);
    for (const name of names.slice(0, 4))
      await expect(pane.card(name).element).toHaveAttribute('aria-selected', 'true');
    await pane.card(names[1]).click(['ControlOrMeta']);
    await expect(pane.card(names[1]).element).toHaveAttribute('aria-selected', 'false');
    if (pane === source) expect(await comparison.card(names[0]).isSelected()).toBe(false);
  }
  expect(await viewer.isOpen()).toBe(false);
});

test('active plain clicks toggle; inactive clicks open viewer; Escape clears', async ({
  source,
  viewer,
}) => {
  await expect.poll(() => source.names()).not.toEqual([]);
  const names = await source.names();
  await source.card(names[0]).select();
  await source.card(names[1]).click();
  expect(await source.card(names[1]).isSelected()).toBe(true);
  expect(await viewer.isOpen()).toBe(false);
  await source.clear();
  await expect(source.card(names[0]).element).toHaveAttribute('aria-selected', 'false');
  await source.card(names[0]).click();
  await viewer.expectOpen(names[0]);
});

test('keyboard and bottom bar select all real folder IDs; navigation clears', async ({
  source,
  comparison,
  bottomBar,
  sharedApp,
  manifest,
  page,
}) => {
  await expect.poll(() => source.names()).not.toEqual([]);
  await source.card((await source.names())[0]).element.focus();
  await page.keyboard.press('ControlOrMeta+A');
  const file = fixture(manifest, 'sidecar.exact');
  const listing = await sharedApp.api.listing(file.volume, file.dir);
  await expect.poll(() => bottomBar.counts('source')).toContain(`${listing.total} selected`);
  await comparison.card((await comparison.names())[0]).element.focus();
  await page.keyboard.press('ControlOrMeta+A');
  await expect.poll(() => bottomBar.counts('comparison')).toContain(`${listing.total} selected`);
  await comparison.clear();
  await bottomBar.selectAll('comparison');
  await expect.poll(() => bottomBar.counts('comparison')).toContain(`${listing.total} selected`);
  await source.element.getByRole('button', { name: 'Up one folder' }).click();
  await expect.poll(() => bottomBar.counts('source')).not.toContain('selected');
  await expect.poll(() => bottomBar.counts('comparison')).toContain(`${listing.total} selected`);
  expect(await comparison.names()).not.toEqual([]);
});

test('select-all does not hijack text input focus', async ({ source, filterBar }) => {
  const input = await filterBar.searchCamera('typed camera');
  await input.press('ControlOrMeta+A');
  expect(
    await input.evaluate(
      (element: HTMLInputElement) => (element.selectionEnd ?? 0) - (element.selectionStart ?? 0),
    ),
  ).toBe(12);
  for (const name of await source.names()) expect(await source.card(name).isSelected()).toBe(false);
});

test('file options have no nested controls and retain keyboard selection and viewer activation', async ({
  source,
  comparison,
  viewer,
  page,
}) => {
  await expect.poll(() => source.names()).not.toEqual([]);
  await expect.poll(() => comparison.names()).not.toEqual([]);
  const names = await source.names();
  await expectNoSeriousA11yViolations(page, { include: ['apm-file-grid'] });
  for (const pane of [source, comparison]) {
    const first = pane.card(names[0]);
    const second = pane.card(names[1]);
    await first.element.focus();
    await expect(first.element).toBeFocused();
    await expect(first.element.locator('[data-checkbox]')).toBeVisible();
    await expect(first.element.locator('input, button, a[href], [tabindex]')).toHaveCount(0);
    await first.element
      .locator('.checkbox')
      .evaluate((indicator: HTMLElement) => indicator.focus());
    await expect(first.element).toBeFocused();
    await page.keyboard.press('Space');
    await expect(first.element).toHaveAttribute('aria-selected', 'true');
    await first.expectBorder('primary');
    await expect(first.element.locator('.checkbox')).toHaveAttribute('aria-checked', 'true');
    await expect
      .poll(() =>
        first.element
          .locator('.checkbox')
          .evaluate((indicator) => getComputedStyle(indicator, '::before').opacity),
      )
      .toBe('1');
    await page.keyboard.press('ArrowRight');
    await expect(second.element).toBeFocused();
    await page.keyboard.press('Space');
    await expect(second.element).toHaveAttribute('aria-selected', 'true');
    expect(await viewer.isOpen()).toBe(false);
    await pane.clear();
    await expect(first.element).toHaveAttribute('aria-selected', 'false');
  }
  await expectNoSeriousA11yViolations(page, { include: ['apm-file-grid'] });
  await source.card(names[0]).element.focus();
  await page.keyboard.press('Enter');
  await viewer.expectOpen(names[0]);
});
