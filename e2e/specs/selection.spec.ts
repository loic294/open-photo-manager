import { expect, test } from '../src/test';

test.beforeEach(async ({ page, sharedApp }) => {
  await page.goto(`${sharedApp.url}/browse?src=photos/sidecars&cmp=photos/sidecars`);
});

test('checkbox, modifier click and shift range select in either pane @smoke', async ({
  source,
  comparison,
  viewer,
}) => {
  await expect.poll(() => source.names()).not.toEqual([]);
  const names = await source.names();
  const first = source.card(names[0]);
  await first.select();
  await expect(first.element).toHaveAttribute('aria-selected', 'true');
  await first.expectBorder('primary');
  await source.card(names[1]).click(['ControlOrMeta']);
  await source.card(names[3]).click(['Shift']);
  for (const name of names.slice(0, 4))
    await expect(source.card(name).element).toHaveAttribute('aria-selected', 'true');
  expect(await comparison.card(names[0]).isSelected()).toBe(false);
  await comparison.card(names[0]).select();
  expect(await comparison.card(names[0]).isSelected()).toBe(true);
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
  page,
}) => {
  await expect.poll(() => source.names()).not.toEqual([]);
  await source.card((await source.names())[0]).element.focus();
  await page.keyboard.press('ControlOrMeta+A');
  const listing = await sharedApp.api.listing('photos', 'sidecars');
  await expect.poll(() => bottomBar.counts('source')).toContain(`${listing.total} selected`);
  await bottomBar.selectAll('comparison');
  await expect.poll(() => bottomBar.counts('comparison')).toContain(`${listing.total} selected`);
  await source.element.getByRole('button', { name: 'Up one folder' }).click();
  await expect.poll(() => bottomBar.counts('source')).not.toContain('selected');
  expect(await comparison.names()).not.toEqual([]);
});

test('select-all does not hijack text input focus', async ({ source, filterBar }) => {
  await filterBar.element.getByRole('button', { name: /^Cameras/ }).click();
  const input = filterBar.element.getByRole('searchbox');
  await input.fill('typed camera');
  await input.press('ControlOrMeta+A');
  expect(
    await input.evaluate(
      (element: HTMLInputElement) => (element.selectionEnd ?? 0) - (element.selectionStart ?? 0),
    ),
  ).toBe(12);
  for (const name of await source.names()) expect(await source.card(name).isSelected()).toBe(false);
});
