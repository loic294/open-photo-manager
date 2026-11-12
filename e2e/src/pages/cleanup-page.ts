import type { Page } from '@playwright/test';

export class CleanupPage {
  constructor(readonly page: Page) {}

  includeSubfolders() {
    return this.page.getByRole('switch', {
      name: 'Include subfolders',
      exact: true,
    });
  }

  actionButton() {
    return this.page.getByRole('button', { name: /^Delete empty folders/ });
  }

  noEmptyFolders() {
    return this.page.getByRole('status', { name: 'No empty folders', exact: true });
  }
}
