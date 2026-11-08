import { fixture } from '../src/scenarios';
import { expect, test } from '../src/test';

// Closing/changing a rendition intentionally cancels pending previews and preloads.
test.use({
  allowedErrors: [/^requestfailed: .*\/api\/files\/\d+\/(?:preview|thumb\?.*) net::ERR_ABORTED$/],
});

for (const key of ['exif.full_nikon', 'media.heic', 'media.video_mp4']) {
  test(`viewer opens real ${key} and restores card focus`, async ({
    page,
    sharedApp,
    source,
    viewer,
    manifest,
  }) => {
    const file = fixture(manifest, key);
    await source.goto(sharedApp.url, file);
    await source.card(file.name).click();
    await viewer.expectOpen(file.name);
    if (file.kind === 'video') {
      await expect(viewer.video()).toHaveAttribute('src', /\/api\/files\/\d+\/preview/);
    } else {
      await expect
        .poll(() => viewer.image().evaluate((image: HTMLImageElement) => image.naturalWidth > 0))
        .toBe(true);
    }
    await page.keyboard.press('Escape');
    await expect(viewer.dialog()).not.toBeVisible();
    await expect(source.card(file.name).element).toBeFocused();
  });
}

test('arrow navigation follows the real folder order; Close dismisses', async ({
  sharedApp,
  source,
  viewer,
  manifest,
}) => {
  const file = fixture(manifest, 'sidecar.exact');
  await source.goto(sharedApp.url, file);
  const listing = await sharedApp.api.listing(file.volume, file.dir);
  await source.card(listing.items[0].name).click();
  await viewer.expectOpen(listing.items[0].name);
  await viewer.next();
  await viewer.expectOpen(listing.items[1].name);
  await viewer.previous();
  await viewer.expectOpen(listing.items[0].name);
  await viewer.close();
});

test('real Chrome can play fixture video', async ({ sharedApp, source, viewer, manifest }) => {
  test.skip(
    process.env.E2E_CHANNEL !== 'chrome',
    'Bundled Chromium does not support H.264; E2E_CHANNEL=chrome enables playback',
  );
  const file = fixture(manifest, 'media.video_mp4');
  await source.goto(sharedApp.url, file);
  await source.card(file.name).click();
  await viewer.video().evaluate((video: HTMLVideoElement) => video.play());
  await expect
    .poll(() => viewer.video().evaluate((video: HTMLVideoElement) => video.currentTime))
    .toBeGreaterThan(0);
});
