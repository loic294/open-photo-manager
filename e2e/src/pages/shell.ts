import { expect, type Page } from '@playwright/test';

export class ShellPage {
  constructor(readonly page: Page) {}
  menu() {
    return this.page.getByRole('navigation', { name: 'Main', exact: true });
  }
  async navigate(name: string) {
    await this.menu().getByRole('link', { name, exact: true }).click();
    await expect(this.page.getByRole('heading', { level: 1, name, exact: true })).toBeVisible();
  }
  topBar() {
    return this.page.getByRole('banner');
  }
  toasts() {
    return this.page.locator('opm-toast-host .alert');
  }
  taskIndicator() {
    return this.page.locator('opm-task-indicator');
  }
  progress() {
    return this.taskIndicator().getByRole('progressbar');
  }
  async hoverTasks() {
    await this.taskIndicator().hover();
  }
  async reindex(all = false) {
    await this.menu()
      .getByRole('button', {
        name: all ? 'Re-index all assets' : 'Re-index current folders',
        exact: true,
      })
      .click();
  }
  async theme(theme: 'light' | 'dark') {
    await this.page
      .locator('html')
      .evaluate((element, value) => element.setAttribute('data-theme', value), theme);
  }

  // testing-e2e-system
  taskTrigger() {
    return this.taskIndicator().getByRole('button').first();
  }
  taskPanel() {
    return this.taskIndicator().getByRole('group', { name: 'Background tasks', exact: true });
  }
  taskRows() {
    return this.taskIndicator().locator('opm-task-row');
  }
  taskRow(title: string) {
    return this.taskRows().filter({
      has: this.page.getByText(title, { exact: true }),
    });
  }
  reindexButton(all = false) {
    return this.menu().getByRole('button', {
      name: all ? 'Re-index all assets' : 'Re-index current folders',
      exact: true,
    });
  }
  notices() {
    return this.page.locator('opm-index-notices');
  }
  connectionBanner() {
    return this.page.getByRole('alert').filter({ hasText: /connection|reconnect/i });
  }
}
