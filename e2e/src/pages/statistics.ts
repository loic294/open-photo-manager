import type { Page } from '@playwright/test';

export class StatisticsPage {
  constructor(readonly page: Page) {}

  root() {
    return this.page.locator('opm-statistics-page');
  }

  async volume(id: number | null) {
    await this.root()
      .getByRole('combobox', { name: 'Statistics volume', exact: true })
      .selectOption(id === null ? '' : String(id));
  }

  volumes() {
    return this.root().getByRole('region', { name: 'Volumes at latest observation', exact: true });
  }

  daily() {
    return this.root().getByRole('region', { name: 'Daily statistics table', exact: true });
  }
}
