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

  // testing-e2e-system
  batches() {
    return this.page.locator('apm-trash-deletions tbody').getByRole('row');
  }
  row(name: string) {
    return this.rows().filter({ has: this.page.getByText(name, { exact: true }) });
  }
  async search(value: string) {
    await this.page.getByRole('searchbox', { name: 'Search the trash', exact: true }).fill(value);
  }
  async volume(id: number | null) {
    await this.page
      .getByRole('combobox', { name: 'Volume', exact: true })
      .selectOption(id === null ? '' : String(id));
  }
  async sort(name: string) {
    await this.page.getByRole('columnheader').getByRole('button', { name, exact: true }).click();
  }
  async next() {
    await this.page.getByRole('button', { name: 'Next page', exact: true }).click();
  }
}
