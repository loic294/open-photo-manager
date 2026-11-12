import type { Page } from '@playwright/test';
import type { Side } from './pane';

export class BottomBarPage {
  readonly element;
  constructor(readonly page: Page) {
    this.element = page.locator('opm-bottom-bar');
  }
  side(side: Side) {
    return this.element.getByRole('group', {
      name: side === 'source' ? 'Source pane' : 'Comparison pane',
      exact: true,
    });
  }
  async selectAll(side: Side) {
    await this.side(side).getByRole('button', { name: 'Select all', exact: true }).click();
  }
  async size(side: Side, value: number) {
    const slider = this.side(side).getByRole('slider', { name: 'Thumbnail size' });
    await slider.fill(String(value));
    await slider.dispatchEvent('input');
  }
  async sizeValue(side: Side) {
    return this.side(side).getByRole('slider', { name: 'Thumbnail size' }).inputValue();
  }
  async counts(side: Side) {
    return this.side(side).textContent();
  }
  splitButton() {
    return this.element.locator('opm-split-button');
  }
  actionButton() {
    return this.splitButton().getByRole('button');
  }
  async actionLabel() {
    return this.actionButton().textContent();
  }
  async openActions() {
    await this.splitButton().locator('summary[aria-label="Choose action"]').click();
  }
  async chooseAction(label: string) {
    await this.openActions();
    await this.splitButton().getByRole('menuitemradio', { name: label, exact: true }).click();
  }
}
