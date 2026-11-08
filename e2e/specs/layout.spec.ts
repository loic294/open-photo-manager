import { browseUrl, fixture } from '../src/scenarios';
import { captureScreens } from '../src/screens';
import { expect, test } from '../src/test';

for (const viewport of [
  { width: 1440, height: 900 },
  { width: 1024, height: 700 },
]) {
  test(`viewport stays fixed with independently scrolling equal panes ${viewport.width} @smoke`, async ({
    page,
    sharedApp,
    source,
    comparison,
    bottomBar,
    shell,
    manifest,
  }) => {
    await page.setViewportSize(viewport);
    const file = fixture(manifest, 'sidecar.exact');
    await page.goto(browseUrl(sharedApp.url, `${file.volume}/${file.dir}`));
    await expect(source.card(file.name).element).toBeVisible();
    const a = await source.element.boundingBox();
    const b = await comparison.element.boundingBox();
    expect(a).not.toBeNull();
    expect(b).not.toBeNull();
    expect(Math.abs((a?.width ?? 0) - (b?.width ?? 0))).toBeLessThanOrEqual(1);
    expect(a?.y).toBe(b?.y);
    expect(b?.x ?? 0).toBeGreaterThan(a?.x ?? 0);
    const top = await shell.topBar().boundingBox();
    const bottom = await bottomBar.element.boundingBox();
    await source.scrollToEnd();
    expect(await source.scrollTop()).toBeGreaterThan(0);
    expect(await comparison.scrollTop()).toBe(0);
    expect(await shell.topBar().boundingBox()).toEqual(top);
    expect(await bottomBar.element.boundingBox()).toEqual(bottom);
    for (const theme of ['light', 'dark'] as const) {
      await shell.theme(theme);
      expect(
        await page.evaluate(() => ({
          height: document.scrollingElement?.scrollHeight,
          viewport: document.scrollingElement?.clientHeight,
        })),
      ).toEqual({ height: viewport.height, viewport: viewport.height });
    }
    if (viewport.width === 1440) await captureScreens(page, 'browse-layout');
  });
}
