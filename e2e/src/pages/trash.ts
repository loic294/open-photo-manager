import type { Page } from '@playwright/test';

export class TrashPage {
  constructor(readonly page: Page) {}
  async tab(name: 'Items' | 'Deletions') {
    await this.page.getByRole('tab', { name, exact: true }).click();
  }
  rows() {
    return this.page.locator('apm-trash-items tbody').getByRole('row');
  }
  selectedRestore() {
    return this.page.getByRole('button', { name: /^Restore selected \(\d+\)$/ });
  }
  async restore() {
    await this.selectedRestore().click();
  }
  emptyButton() {
    return this.page.getByRole('button', { name: /^Empty trash/ });
  }
  async empty() {
    await this.emptyButton().click();
  }
  async openActions() {
    await this.page.getByRole('button', { name: 'More actions', exact: true }).click();
  }
  syncButton() {
    return this.page.getByRole('button', { name: 'Look for untracked items', exact: true });
  }
  async sync() {
    await this.openActions();
    await this.syncButton().click();
  }
}
