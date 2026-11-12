import { expectNoSeriousA11yViolations } from '../../src/a11y';
import { expect, test } from '../../src/test';

for (const theme of ['light', 'dark'] as const) {
  test(`DaisyUI error colors remain readable on ${theme} surfaces`, async ({
    sharedApp,
    page,
  }, info) => {
    await page.addInitScript(
      (value) => localStorage.setItem('opm.v1.theme', JSON.stringify(value)),
      theme,
    );
    await page.goto(sharedApp.url);
    await page.evaluate(() => {
      const fixture = document.createElement('div');
      fixture.id = 'error-theme-fixture';
      fixture.innerHTML = ['100', '200', '300']
        .map(
          (level) => `<section style="background:var(--color-base-${level})">
            <span class="text-error" data-kind="text">Error text</span>
            <button class="btn btn-error btn-outline btn-sm" data-kind="outline">Error outline</button>
            <button class="btn btn-error btn-sm" data-kind="button">Error button</button>
            <span class="badge badge-error" data-kind="badge">Error badge</span>
            <div class="alert alert-error" data-kind="alert">Error alert</div>
            <div class="alert alert-error alert-soft" data-kind="soft">Soft error alert</div>
            <input class="input input-error" aria-label="Invalid input" data-kind="input">
          </section>`,
        )
        .join('');
      document.body.append(fixture);
    });

    const results = await page
      .locator('#error-theme-fixture [data-kind]')
      .evaluateAll((elements) => {
        const canvas = document.createElement('canvas');
        canvas.width = canvas.height = 1;
        const context = canvas.getContext('2d', { willReadFrequently: true });
        if (!context) throw new Error('Canvas unavailable');
        const pixel = (background: string, foreground?: string) => {
          context.clearRect(0, 0, 1, 1);
          context.fillStyle = background;
          context.fillRect(0, 0, 1, 1);
          if (foreground) {
            context.fillStyle = foreground;
            context.fillRect(0, 0, 1, 1);
          }
          return Array.from(context.getImageData(0, 0, 1, 1).data).slice(0, 3);
        };
        const luminance = (rgb: number[]) =>
          rgb
            .map((channel) => {
              const value = channel / 255;
              return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
            })
            .reduce((sum, channel, index) => sum + channel * [0.2126, 0.7152, 0.0722][index], 0);
        return elements.map((element) => {
          const style = getComputedStyle(element);
          const surface = getComputedStyle(element.parentElement as Element).backgroundColor;
          const background = pixel(surface, style.backgroundColor);
          const backgroundCss = `rgb(${background.join(' ')})`;
          const actualForeground = pixel(
            backgroundCss,
            element.getAttribute('data-kind') === 'input' ? style.borderTopColor : style.color,
          );
          const a = luminance(actualForeground);
          const b = luminance(background);
          return {
            kind: element.getAttribute('data-kind'),
            surface,
            color: style.color,
            border: style.borderTopColor,
            background: style.backgroundColor,
            ratio: (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05),
          };
        });
      });
    await info.attach('error-theme-contrast', {
      body: JSON.stringify({ theme, results }, null, 2),
      contentType: 'application/json',
    });
    console.log(JSON.stringify({ theme, results }));
    for (const result of results) {
      expect(result.ratio, JSON.stringify(result)).toBeGreaterThanOrEqual(
        result.kind === 'input' ? 3 : 4.5,
      );
    }
    for (const outline of await page.locator('#error-theme-fixture [data-kind="outline"]').all()) {
      await outline.hover();
      await expectNoSeriousA11yViolations(page, { include: ['#error-theme-fixture'] });
      await outline.focus();
      await expectNoSeriousA11yViolations(page, { include: ['#error-theme-fixture'] });
    }
  });
}
