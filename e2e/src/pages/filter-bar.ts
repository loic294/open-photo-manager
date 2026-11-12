import { expect, type Page } from '@playwright/test';

export class FilterBarPage {
  readonly element;
  constructor(readonly page: Page) {
    this.element = page.getByRole('group', { name: 'Filters', exact: true });
  }
  async open() {
    const toggle = this.page.getByRole('button', { name: 'Toggle filters', exact: true });
    if ((await toggle.getAttribute('aria-expanded')) === 'false') await toggle.click();
    await expect(this.element).toBeVisible();
  }
  async pick(label: 'Cameras' | 'Tags', value: string) {
    await this.open();
    const control = this.element.locator(`opm-multi-select[label="${label}"]`);
    await control.locator('summary').click();
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
    await this.open();
    await this.element
      .getByRole('group', { name: 'Rating', exact: true })
      .getByRole('button', { name: new RegExp(`^${value === 0 ? 'Unrated' : `${value} star`}`) })
      .click();
  }
  async flag(value: 'Selected' | 'Rejected') {
    await this.open();
    await this.element
      .getByRole('group', { name: 'Flags', exact: true })
      .getByRole('button', { name: new RegExp(`^${value}`) })
      .click();
  }
  async clear() {
    await this.open();
    await this.element.getByRole('button', { name: /^Clear filters/ }).click();
  }
  async waitForCountedFacets() {
    await this.open();
    await expect(this.element.getByRole('button', { name: /^Unrated \(\d+\)$/ })).toBeVisible();
  }
  async hideUnmatched(value: boolean) {
    await this.open();
    await this.element
      .getByRole('switch', { name: 'Hide folders without matches' })
      .setChecked(value);
  }
  async scope(value: string) {
    await this.open();
    await this.element.getByRole('combobox', { name: 'Apply to' }).selectOption(value);
  }
  facets(label: 'Cameras' | 'Tags') {
    return this.element.locator(`opm-multi-select[label="${label}"]`).getByRole('checkbox');
  }
  async searchCamera(value: string) {
    await this.open();
    await this.element.locator('opm-multi-select[label="Cameras"] summary').click();
    const input = this.element.getByRole('searchbox');
    await input.fill(value);
    return input;
  }
}
