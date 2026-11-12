import AxeBuilder from '@axe-core/playwright';
import { expectNoSeriousA11yViolations } from '../../src/a11y';
import { expect, test } from '../../src/test';

async function openDuplicateConfirmation(
  sharedApp: { url: string },
  page: import('@playwright/test').Page,
  duplicatesPage: import('../../src/pages/duplicates-page').DuplicatesPage,
) {
  const plan = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/plans',
  );
  await page.goto(new URL('/duplicates?src=photos/dupes/m1&cmp=archive/m1', sharedApp.url).href);
  await plan;
  await expect(page.locator('opm-pane [data-pane-body][aria-busy="true"]')).toHaveCount(0);
  await duplicatesPage.actionButton().click();
}

test('confirmation content is opaque on its first visible frame', async ({
  sharedApp,
  page,
  duplicatesPage,
  confirmDialog,
}) => {
  await openDuplicateConfirmation(sharedApp, page, duplicatesPage);
  await expect(confirmDialog.dialog()).toBeVisible();
  await confirmDialog.cancel();
  await expect(confirmDialog.dialog()).not.toBeVisible();
  const nativeDialog = page.locator('opm-confirm-dialog dialog.modal');
  await nativeDialog.evaluate((dialog) => {
    const element = dialog as HTMLDialogElement;
    const box = element.querySelector<HTMLElement>('.modal-box');
    if (!box) throw new Error('Confirmation modal box is not rendered');
    const firstFrame = new Promise<{ dialogOpacity: string; boxOpacity: string }>((resolve) => {
      const observer = new MutationObserver(() => {
        if (!element.open) return;
        observer.disconnect();
        requestAnimationFrame(() =>
          resolve({
            dialogOpacity: getComputedStyle(element).opacity,
            boxOpacity: getComputedStyle(box).opacity,
          }),
        );
      });
      observer.observe(element, { attributes: true, attributeFilter: ['open'] });
    });
    (element as HTMLDialogElement & { firstVisibleFrame: typeof firstFrame }).firstVisibleFrame =
      firstFrame;
  });
  await duplicatesPage.actionButton().click();
  const firstFrame = await nativeDialog.evaluate(async (dialog) => {
    const element = dialog as HTMLDialogElement & {
      firstVisibleFrame: Promise<{ dialogOpacity: string; boxOpacity: string }>;
    };
    return element.firstVisibleFrame;
  });
  expect(firstFrame).toEqual({ dialogOpacity: '1', boxOpacity: '1' });
});

test('confirmation warning has evaluated contrast and its backdrop still dismisses', async ({
  sharedApp,
  page,
  duplicatesPage,
  confirmDialog,
}, testInfo) => {
  await openDuplicateConfirmation(sharedApp, page, duplicatesPage);
  await expect(confirmDialog.dialog()).toBeVisible();
  await confirmDialog.dialog().evaluate(async (dialog) => {
    await Promise.all(
      dialog.getAnimations({ subtree: true }).map((animation) => animation.finished),
    );
  });
  const analysis = await new AxeBuilder({ page }).analyze();
  const contrast = {
    violations: analysis.violations.filter((rule) => rule.id === 'color-contrast'),
    incomplete: analysis.incomplete.filter((rule) => rule.id === 'color-contrast'),
    passes: analysis.passes.filter((rule) => rule.id === 'color-contrast'),
  };
  await testInfo.attach('confirmation-contrast', {
    body: JSON.stringify(contrast, null, 2),
    contentType: 'application/json',
  });
  const messageNodes = (rules: typeof contrast.passes) =>
    rules
      .flatMap((rule) => rule.nodes)
      .filter((node) => node.target.includes('#opm-confirm-message'));
  expect(messageNodes(contrast.incomplete)).toEqual([]);
  expect(messageNodes(contrast.violations)).toEqual([]);
  expect(messageNodes(contrast.passes)).toHaveLength(1);
  await expectNoSeriousA11yViolations(page);
  const backdrop = confirmDialog.dialog().locator('.modal-backdrop button');
  expect((await backdrop.textContent())?.trim()).toBe('');
  await backdrop.click({ position: { x: 2, y: 2 } });
  await expect(confirmDialog.dialog()).not.toBeVisible();
});
