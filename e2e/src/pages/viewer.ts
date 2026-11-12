import { expect, type Page } from '@playwright/test';

export class ViewerPage {
  constructor(readonly page: Page) {}
  dialog() {
    return this.page.locator('opm-viewer').getByRole('dialog');
  }
  async expectOpen(name: string) {
    await expect(this.dialog()).toBeVisible();
    await expect(this.dialog()).toContainText(name);
  }
  async isOpen() {
    return this.dialog().isVisible();
  }
  async name() {
    return this.dialog().getByRole('heading').first().textContent();
  }
  image() {
    return this.dialog().locator('opm-viewer-stage img');
  }
  video() {
    return this.dialog().locator('video');
  }
  async next() {
    await this.page.keyboard.press('ArrowRight');
  }
  async previous() {
    await this.page.keyboard.press('ArrowLeft');
  }
  async close() {
    // The product deliberately swallows the second click of a double-click while opening.
    await expect
      .poll(() =>
        this.page
          .locator('opm-viewer')
          .evaluate((element) => 'guarded' in element && element.guarded === false),
      )
      .toBe(true);
    await this.dialog().getByRole('button', { name: 'Close', exact: true }).click();
    await expect(this.dialog()).not.toBeVisible();
  }
}
