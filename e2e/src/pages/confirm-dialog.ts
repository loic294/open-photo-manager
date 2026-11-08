import { expect, type Page } from '@playwright/test';

export class ConfirmDialogPage {
  constructor(readonly page: Page) {}
  dialog() {
    return this.page.locator('apm-confirm-dialog').getByRole('dialog');
  }
  async confirm(label: string) {
    await this.dialog().getByRole('button', { name: label, exact: true }).click();
    await expect(this.dialog()).not.toBeVisible();
  }
  async cancel() {
    await this.dialog().getByRole('button', { name: 'Cancel', exact: true }).click();
  }
  async text() {
    return this.dialog().textContent();
  }
}
