import { expect, type Page } from '@playwright/test';
import { FileCard } from './pane';

export class MovePage {
  constructor(readonly page: Page) {}

  moveType() {
    return this.page.getByRole('combobox', { name: 'Move type', exact: true });
  }

  async chooseMoveType(value: 'keep_source' | 'keep_destination' | 'flat' | 'custom') {
    await this.moveType().selectOption(value);
  }

  matchMode() {
    return this.page.getByRole('combobox', { name: 'Match by', exact: true });
  }

  policy() {
    return this.page.getByRole('combobox', {
      name: 'If a file with the same name exists',
      exact: true,
    });
  }

  async choosePolicy(value: 'default' | 'smart' | 'skip' | 'rename' | 'replace') {
    await this.policy().selectOption(value);
  }

  pattern() {
    return this.page.getByRole('textbox', { name: 'Folder pattern', exact: true });
  }

  async openPattern() {
    await this.page
      .locator('details.collapse > summary')
      .filter({ hasText: 'Folder pattern:' })
      .click();
  }

  actionButton() {
    return this.page.getByRole('button', { name: /^Move files/ });
  }

  planNotices() {
    return this.page
      .getByRole('status')
      .filter({ hasText: /Choose the (source|destination) folder/ });
  }

  sourceBase() {
    return this.page.getByRole('region', { name: 'Source' }).getByRole('group', {
      name: 'Base folder',
    });
  }

  destinationBase() {
    return this.destinationPane().getByRole('group', {
      name: 'Base folder',
    });
  }

  destinationPane() {
    return this.page.getByRole('region', { name: 'Destination', exact: true });
  }

  destinationCard(name: string) {
    return new FileCard(this.destinationPane(), name);
  }

  destinationFolder(name: string) {
    return this.destinationPane()
      .locator('apm-folder-list a[data-name]')
      .filter({ has: this.page.getByText(name, { exact: true }) });
  }

  async openDestinationFolder(name: string) {
    await this.destinationFolder(name).click();
    await expect(this.destinationPane().locator('[data-pane-body]')).not.toHaveAttribute(
      'aria-busy',
      'true',
    );
  }
}
