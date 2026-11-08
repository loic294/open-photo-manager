import type { Page } from '@playwright/test';

export class SettingsPage {
  constructor(readonly page: Page) {}
  async tab(name: string) {
    await this.page.getByRole('tab', { name, exact: true }).click();
  }
  field(name: string) {
    return this.page.getByLabel(name, { exact: true });
  }
  async fill(name: string, value: string) {
    await this.field(name).fill(value);
  }
  async save() {
    await this.page.getByRole('button', { name: 'Save', exact: true }).click();
  }
}
