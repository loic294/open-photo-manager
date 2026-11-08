import { expectNoSeriousA11yViolations } from '../../src/a11y';
import { expect, test } from '../../src/test';

test('Sidebar and Trash text retain contrast through focused theme changes', async ({
  sharedApp,
  page,
}, info) => {
  await page.goto(`${sharedApp.url}/trash`);
  await expect(page.locator('apm-trash-page .alert-info')).toBeVisible();
  const settings = page.getByRole('navigation', { name: 'Main', exact: true }).getByRole('link', {
    name: 'Settings',
    exact: true,
  });
  await settings.hover();
  await settings.focus();

  const samples = await page.evaluate(async () => {
    const canvas = document.createElement('canvas');
    canvas.width = canvas.height = 1;
    const context = canvas.getContext('2d', { willReadFrequently: true });
    if (!context) throw new Error('Cannot measure rendered sRGB colours');
    const pixel = (color: string) => {
      context.clearRect(0, 0, 1, 1);
      context.fillStyle = color;
      context.fillRect(0, 0, 1, 1);
      return [...context.getImageData(0, 0, 1, 1).data];
    };
    const composite = (foreground: number[], background: number[]) => {
      const alpha = foreground[3] / 255;
      return foreground.slice(0, 3).map((value, index) => {
        return value * alpha + background[index] * (1 - alpha);
      });
    };
    const luminance = (rgb: number[]) =>
      rgb
        .map((value) => {
          const channel = value / 255;
          return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
        })
        .reduce((sum, value, index) => sum + value * [0.2126, 0.7152, 0.0722][index], 0);
    const selectors = [
      '#apm-menu a[href$="browse"]',
      '#apm-menu a[href$="settings"]',
      '#apm-menu li > button',
    ];
    const result = [];
    for (const theme of ['light', 'dark', 'light']) {
      document.documentElement.setAttribute('data-theme', theme);
      for (let frame = 0; frame < 20; frame++) {
        result.push({
          theme,
          frame,
          nodes: selectors.flatMap((selector) =>
            [...document.querySelectorAll(selector)].map((element) => {
              const style = getComputedStyle(element);
              const parentBackground = getComputedStyle(
                document.querySelector('#apm-menu') ?? document.documentElement,
              ).backgroundColor;
              const background = composite(pixel(style.backgroundColor), pixel(parentBackground));
              const foreground = composite(pixel(style.color), background);
              const bgLuminance = luminance(background);
              const fgLuminance = luminance(foreground);
              return {
                selector,
                text: element.textContent?.trim(),
                foreground: style.color,
                background: style.backgroundColor,
                parentBackground,
                effectiveForeground: foreground,
                effectiveBackground: background,
                contrast:
                  (Math.max(bgLuminance, fgLuminance) + 0.05) /
                  (Math.min(bgLuminance, fgLuminance) + 0.05),
                opacity: style.opacity,
                transitionProperty: style.transitionProperty,
                animations: element.getAnimations().map((animation) => ({
                  property:
                    animation instanceof CSSTransition ? animation.transitionProperty : null,
                  currentTime: animation.currentTime,
                  playState: animation.playState,
                })),
              };
            }),
          ),
        });
        await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      }
    }
    return result;
  });
  await info.attach('sidebar-theme-styles', {
    body: JSON.stringify(samples, null, 2),
    contentType: 'application/json',
  });
  expect(
    samples.flatMap((sample) =>
      sample.nodes.flatMap((node) =>
        node.animations.filter(
          (animation) =>
            animation.property === 'color' || animation.property === 'background-color',
        ),
      ),
    ),
    'Foreground and background must switch together, not animate through unreadable midtones',
  ).toEqual([]);
  expect(
    Math.min(...samples.flatMap((sample) => sample.nodes.map((node) => node.contrast))),
    'Normal-size sidebar text must stay at least 4.5:1 throughout theme changes',
  ).toBeGreaterThanOrEqual(4.5);

  for (const theme of ['light', 'dark'] as const) {
    await page.locator('html').evaluate((element, value) => {
      element.setAttribute('data-theme', value);
    }, theme);
    await expectNoSeriousA11yViolations(page);
    await page.getByRole('tab', { name: 'Items', exact: true }).focus();
    await expectNoSeriousA11yViolations(page);
  }
});
