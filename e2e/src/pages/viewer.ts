import { expect, type Page } from '@playwright/test';

export class ViewerPage {
  constructor(readonly page: Page) {}
  dialog() {
    return this.page.locator('apm-viewer').getByRole('dialog');
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
    return this.dialog().locator('apm-viewer-stage img');
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
    await this.dialog().getByRole('button', { name: 'Close', exact: true }).click();
    await expect(this.dialog()).not.toBeVisible();
  }
}
