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
    return this.page.locator('apm-top-bar');
  }
  toasts() {
    return this.page.locator('apm-toast');
  }
  taskIndicator() {
    return this.page.locator('apm-task-indicator');
  }
  progress() {
    return this.taskIndicator().getByRole('progressbar');
  }
  async hoverTasks() {
    await this.taskIndicator().hover();
  }
  async reindex(all = false) {
    await this.menu().getByRole('button', { name: 'Re-index', exact: true }).click();
    await this.menu()
      .getByRole('button', { name: all ? 'All assets' : 'Current folders', exact: true })
      .click();
  }
  async theme(theme: 'light' | 'dark') {
    await this.page
      .locator('html')
      .evaluate((element, value) => element.setAttribute('data-theme', value), theme);
  }
}
