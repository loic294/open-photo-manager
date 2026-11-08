import type { Page } from '@playwright/test';

export class FilterBarPage {
  readonly element;
  constructor(readonly page: Page) {
    this.element = page.getByRole('group', { name: 'Filters', exact: true });
  }
  async pick(label: 'Cameras' | 'Tags', value: string) {
    const control = this.element
      .locator('apm-multi-select')
      .filter({ has: this.page.getByRole('button', { name: new RegExp(`^${label}`) }) });
    await control.getByRole('button', { name: new RegExp(`^${label}`) }).click();
    await control
      .getByRole('checkbox', {
        name: new RegExp(`^${value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}( \\(|$)`),
      })
      .check();
    await this.page.keyboard.press('Escape');
  }
  async camera(value: string) {
    await this.pick('Cameras', value);
  }
  async tag(value: string) {
    await this.pick('Tags', value);
  }
  async rating(value: number) {
    await this.element
      .getByRole('group', { name: 'Rating', exact: true })
      .getByRole('button', { name: new RegExp(`^${value === 0 ? 'Unrated' : `${value} star`}`) })
      .click();
  }
  async flag(value: 'Selected' | 'Rejected') {
    await this.element
      .getByRole('group', { name: 'Flags', exact: true })
      .getByRole('button', { name: new RegExp(`^${value}`) })
      .click();
  }
  async clear() {
    await this.element.getByRole('button', { name: /^Clear filters/ }).click();
  }
  async hideUnmatched(value: boolean) {
    await this.element
      .getByRole('switch', { name: 'Hide folders without matches' })
      .setChecked(value);
  }
  async scope(value: string) {
    await this.element.getByRole('combobox', { name: 'Apply to' }).selectOption(value);
  }
  facets(label: 'Cameras' | 'Tags') {
    return this.element.locator(`apm-multi-select[label="${label}"]`).getByRole('checkbox');
  }
}
