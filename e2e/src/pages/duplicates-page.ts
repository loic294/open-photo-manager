import type { Locator, Page } from '@playwright/test';

export class DuplicatesPage {
  constructor(readonly page: Page) {}

  mode() {
    return this.page.getByRole('combobox', { name: 'Match by', exact: true });
  }

  async chooseMode(label: string) {
    await this.mode().selectOption({ label });
  }

  strictPrefix() {
    return this.page.getByRole('checkbox', { name: 'Strict prefix', exact: true });
  }

  actionButton() {
    return this.page.locator('opm-bottom-bar opm-split-button button.btn-lg').first();
  }

  async actionLabel() {
    return (await this.actionButton().textContent())?.trim() ?? '';
  }

  async chooseAction(label: string) {
    const split = this.page.locator('opm-bottom-bar opm-split-button');
    await split.locator('summary[aria-label="Choose action"]').click();
    await split.getByRole('menuitemradio', { name: label, exact: true }).click();
  }

  planSummary(): Locator {
    return this.page
      .getByRole('status')
      .filter({ hasText: /files? have a match|No duplicates found/ });
  }

  actionMenu() {
    return this.page.locator('opm-bottom-bar opm-split-button');
  }
}
