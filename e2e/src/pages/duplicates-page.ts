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
    return this.page.locator('opm-bottom-bar opm-split-button .join > button').first();
  }

  async actionLabel() {
    return (await this.actionButton().textContent())?.trim() ?? '';
  }

  async chooseAction(label: string) {
    const split = this.actionMenu();
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

  files() {
    return this.page.locator('opm-duplicate-files');
  }

  async showFiles() {
    await this.page.getByRole('button', { name: 'File view', exact: true }).click();
  }

  async findFiles() {
    const response = this.page.waitForResponse((response) =>
      new URL(response.url()).pathname.endsWith('/duplicate-files'),
    );
    await this.files().getByRole('button', { name: 'Find duplicates', exact: true }).click();
    return response;
  }

  automaticDialog() {
    return this.page.locator('opm-duplicate-selection').getByRole('dialog');
  }

  folderPicker(heading: string) {
    return this.page.getByRole('dialog', { name: heading, exact: true });
  }

  async pickFolder(heading: string, folders: string[]) {
    const dialog = this.folderPicker(heading);
    for (const folder of folders) {
      await dialog
        .locator('opm-folder-list a[data-name]')
        .filter({ has: this.page.getByText(folder, { exact: true }) })
        .click();
    }
    await dialog.getByRole('button', { name: 'Use this folder', exact: true }).click();
  }
}
