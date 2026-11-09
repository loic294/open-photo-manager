import { expect, test } from '../../src/test';

for (const theme of ['light', 'dark'] as const) {
  test(`warning text is readable on ${theme} surfaces without changing amber fills`, async ({
    app,
    page,
  }, info) => {
    await page.addInitScript(
      (value) => localStorage.setItem('apm.v1.theme', JSON.stringify(value)),
      theme,
    );
    await page.goto(app.url);
    await page.evaluate(() => {
      const fixture = document.createElement('div');
      fixture.id = 'warning-theme-fixture';
      fixture.innerHTML = `
        <section style="background:var(--color-base-100)">
          <span class="text-warning" data-kind="plain">Plain warning text</span>
          <div class="alert alert-warning alert-soft" data-kind="soft-alert">Soft warning alert</div>
          <div class="alert alert-warning alert-outline" data-kind="outline-alert">Outline warning alert</div>
          <span class="badge badge-warning badge-outline" data-kind="outline-badge">Warning badge</span>
          <span class="badge badge-warning badge-soft" data-kind="soft-badge">Soft warning badge</span>
          <div class="alert alert-warning" data-kind="solid-alert">Solid warning alert</div>
          <span class="badge badge-warning" data-kind="solid-badge">Solid warning badge</span>
        </section>`;
      document.body.append(fixture);
    });

    const results = await page
      .locator('#warning-theme-fixture [data-kind]')
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
        const warning = getComputedStyle(document.documentElement)
          .getPropertyValue('--color-warning')
          .trim();
        const warningContent = getComputedStyle(document.documentElement)
          .getPropertyValue('--color-warning-content')
          .trim();
        const normalizedColor = (color: string) => pixel('rgb(0 0 0)', color);
        const warningRgb = normalizedColor(warning);
        const warningContentRgb = normalizedColor(warningContent);
        const swatch = document.createElement('span');
        swatch.style.backgroundColor = 'var(--color-warning)';
        elements[0].parentElement?.append(swatch);
        const warningFill = getComputedStyle(swatch).backgroundColor;
        swatch.remove();

        return elements.map((element) => {
          const style = getComputedStyle(element);
          const surface = getComputedStyle(element.parentElement as Element).backgroundColor;
          const background = pixel(surface, style.backgroundColor);
          const backgroundCss = `rgb(${background.join(' ')})`;
          const foreground = pixel(backgroundCss, style.color);
          const ratio =
            (Math.max(luminance(foreground), luminance(background)) + 0.05) /
            (Math.min(luminance(foreground), luminance(background)) + 0.05);
          const kind = element.getAttribute('data-kind');
          return {
            kind,
            color: style.color,
            background: style.backgroundColor,
            border: style.borderTopColor,
            ratio,
            foregroundRgb: normalizedColor(style.color),
            warningContentRgb,
            warningFill,
            warningRgb,
            warningToken: warning,
          };
        });
      });
    await info.attach('warning-theme-contrast', {
      body: JSON.stringify({ theme, results }, null, 2),
      contentType: 'application/json',
    });
    for (const result of results) {
      expect(result.ratio, JSON.stringify(result)).toBeGreaterThanOrEqual(4.5);
      if (result.kind === 'solid-badge') {
        expect(result.background).toBe(result.warningFill);
      }
      if (theme === 'dark' && !result.kind?.startsWith('solid-')) {
        const expected =
          result.kind === 'outline-alert' ? result.warningContentRgb : result.warningRgb;
        expect(result.foregroundRgb).toEqual(expected);
      }
    }
    const outlineBadge = results.find((result) => result.kind === 'outline-badge');
    expect(outlineBadge?.border).toBe(outlineBadge?.warningFill);
  });
}
