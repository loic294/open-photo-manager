import { expect, type Page } from '@playwright/test';
import { FileCard } from './pane';

export class FormatPage {
  constructor(readonly page: Page) {}

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
    return this.page.locator('apm-pattern-editor details').filter({ hasText: 'Live preview' });
  }

  variables() {
    return this.page
      .locator('apm-pattern-editor summary')
      .filter({ hasText: 'Variables and examples' });
  }

  previewRows() {
    return this.livePreview().locator('tbody tr');
  }

  planNotices() {
    return this.page.locator('apm-tool-view [role="status"], apm-tool-view [role="alert"]');
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
