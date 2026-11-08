import type { Page } from '@playwright/test';

export class TrashPage {
  constructor(readonly page: Page) {}
  rows() {
    return this.page.locator('apm-trash-items').getByRole('row');
  }
  async restore() {
    await this.page.getByRole('button', { name: /^Restore/ }).click();
  }
  async empty() {
    await this.page.getByRole('button', { name: /^Empty trash/ }).click();
  }
  async sync() {
    await this.page.getByRole('button', { name: /sync|reconcile/i }).click();
  }
}
