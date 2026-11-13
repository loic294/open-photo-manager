import { expect, type Page } from '@playwright/test';

export class RenamePage {
  constructor(readonly page: Page) {}

  async pattern(value: string) {
    await this.page
      .locator('summary')
      .filter({ hasText: /^Rename rules/ })
      .click();
    const input = this.page.getByRole('textbox', { name: 'Name pattern', exact: true });
    await expect(input).toBeVisible();
    await input.fill(value);
  }
}
