import { expect, type Page } from '@playwright/test';

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

  // testing-e2e-system
  general() {
    return this.page.getByRole('form', { name: 'General settings' });
  }
  saveButton() {
    return this.general().getByRole('button', { name: 'Save', exact: true });
  }
  async saveGeneral() {
    await this.save();
    await expect(this.saveButton()).toHaveCount(0);
  }
  async discard() {
    await this.general().getByRole('button', { name: 'Discard', exact: true }).click();
  }
  async toggle(name: string, checked: boolean) {
    await this.general().getByRole('switch', { name, exact: true }).setChecked(checked);
  }
  async addChip(label: string, value: string) {
    const field = this.general().locator(`apm-chip-input[label="${label}"]`);
    await field.getByRole('textbox').fill(value);
    await field.getByRole('textbox').press('Enter');
  }
  async removeChip(label: string, value: string) {
    await this.general()
      .locator(`apm-chip-input[label="${label}"]`)
      .getByRole('button', { name: `Remove ${value}`, exact: true })
      .click();
  }
  volumes() {
    return this.general().getByRole('group', { name: 'Volumes', exact: true });
  }
}
