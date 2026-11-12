import { expect, type Page } from '@playwright/test';
import { FileCard } from './pane';

export class FormatPage {
  constructor(readonly page: Page) {}

  async edit() {
    await this.page.getByRole('button', { name: 'Edit formatting', exact: true }).click();
    await expect(
      this.page.getByRole('dialog', { name: 'Edit formatting', exact: true }),
    ).toBeVisible();
  }

  async done() {
    await this.page
      .getByRole('dialog', { name: 'Edit formatting', exact: true })
      .getByRole('button', { name: 'Done', exact: true })
      .click();
    await expect(
      this.page.getByRole('dialog', { name: 'Edit formatting', exact: true }),
    ).not.toBeVisible();
  }

  pattern() {
    return this.page.getByRole('textbox', { name: 'Folder pattern', exact: true });
  }

  policy() {
    return this.page.getByRole('combobox', {
      name: 'If a file with the same name exists',
      exact: true,
    });
  }

  actionButton() {
    return this.page.getByRole('button', { name: /^Move and format files/ });
  }

  livePreview() {
    return this.page.locator('opm-pattern-editor details').filter({ hasText: 'Live preview' });
  }

  variables() {
    return this.page
      .locator('opm-pattern-editor summary')
      .filter({ hasText: 'Variables and examples' });
  }

  previewRows() {
    return this.livePreview().locator('tbody tr');
  }

  planNotices() {
    return this.page.locator('opm-tool-view [role="status"], opm-tool-view [role="alert"]');
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
      .locator('opm-folder-list a[data-name]')
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
