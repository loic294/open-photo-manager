import { expect, type Locator, type Page } from '@playwright/test';
import type { FixtureFile } from '../manifest';

export type Side = 'source' | 'comparison';

export class FileCard {
  readonly element: Locator;
  constructor(
    pane: Locator,
    readonly name: string,
  ) {
    this.element = pane
      .getByRole('option')
      .filter({ has: pane.page().getByText(name, { exact: true }) });
  }
  async click(modifiers: ('ControlOrMeta' | 'Shift')[] = []) {
    await this.element.click({ modifiers });
  }
  async select() {
    await this.element.hover();
    await this.element.locator('[data-checkbox]').click();
  }
  async expectCheckboxTopRight() {
    await this.element.hover();
    const image = await this.element.locator('figure').boundingBox();
    const checkbox = await this.element.locator('.checkbox').boundingBox();
    expect(image).not.toBeNull();
    expect(checkbox).not.toBeNull();
    expect(checkbox?.x ?? 0).toBeGreaterThan((image?.x ?? 0) + (image?.width ?? 0) * 0.75);
    expect(checkbox?.y ?? Infinity).toBeLessThan((image?.y ?? 0) + (image?.height ?? 0) * 0.25);
  }
  async isSelected() {
    return (await this.element.getAttribute('aria-selected')) === 'true';
  }
  async isGhost() {
    return (await this.element.getAttribute('aria-disabled')) === 'true';
  }
  async isMatched() {
    return (await this.element.getAttribute('aria-label'))?.includes(', matched') ?? false;
  }
  date() {
    return this.element.locator('[data-date]');
  }
  thumbnail() {
    return this.element.locator('img');
  }
  async expectThumbnail() {
    await expect
      .poll(() =>
        this.thumbnail().evaluate(
          (image: HTMLImageElement) => image.complete && image.naturalWidth > 0,
        ),
      )
      .toBe(true);
    await expect(this.thumbnail()).toHaveAttribute(
      'src',
      /\/api\/files\/\d+\/thumb\?size=(sm|lg)&v=/,
    );
  }
  async expectBorder(token: 'primary' | 'success' | 'error') {
    await expect
      .poll(() =>
        this.element.evaluate((element, name) => {
          const reference = document.createElement('span');
          reference.style.borderColor = `var(--color-${name})`;
          element.append(reference);
          const same =
            getComputedStyle(element).borderTopColor === getComputedStyle(reference).borderTopColor;
          reference.remove();
          return same;
        }, token),
      )
      .toBe(true);
  }
}

export class PanePage {
  readonly element: Locator;
  readonly body: Locator;
  constructor(
    readonly page: Page,
    readonly side: Side,
  ) {
    this.element = page.getByRole('region', {
      name: side === 'source' ? 'Source' : /^(Comparison|Destination)$/,
      exact: true,
    });
    this.body = this.element.locator('[data-pane-body]');
  }
  card(name: string) {
    return new FileCard(this.element, name);
  }
  folders() {
    return this.element.locator('opm-folder-list a[data-name]');
  }
  folder(name: string) {
    return this.folders().filter({
      has: this.page.getByText(name, { exact: true }),
    });
  }
  async openFolder(name: string) {
    await this.folder(name).click();
    await this.waitReady();
  }
  async waitReady() {
    await expect(this.body).not.toHaveAttribute('aria-busy', 'true');
  }
  async folderNames() {
    return this.folders().evaluateAll((rows) => rows.map((row) => row.getAttribute('data-name')));
  }
  async folderState(name: string) {
    return this.folder(name).evaluate((row) => ({
      greyed: row.classList.contains('opacity-50'),
      red: row.classList.contains('border-error'),
      planned: row.classList.contains('border-dashed'),
      count: row.querySelector('.badge[aria-hidden="true"]')?.textContent?.trim(),
      matches: [...row.querySelectorAll('.badge')]
        .find((badge) => /\bmatches?\b/.test(badge.textContent ?? ''))
        ?.textContent?.trim(),
      classes: row.className,
      text: row.textContent,
    }));
  }
  async folderGeometry(name: string) {
    return this.folder(name).evaluate((row) => {
      const label = row.querySelector('span[title]')?.getBoundingClientRect();
      const count = row.querySelector('.badge[aria-hidden="true"]')?.getBoundingClientRect();
      return { nameRight: label?.right, countLeft: count?.left };
    });
  }
  async foldersBeforeFiles() {
    return this.body.evaluate((body) => {
      const folders = body.querySelector('opm-folder-list');
      const files = body.querySelector('opm-file-grid');
      return (
        folders !== null &&
        files !== null &&
        Boolean(folders.compareDocumentPosition(files) & Node.DOCUMENT_POSITION_FOLLOWING)
      );
    });
  }
  breadcrumbs() {
    return this.element.getByRole('navigation');
  }
  async useFolder() {
    await this.element.getByRole('button', { name: 'Use this folder', exact: true }).click();
  }
  async reset() {
    await this.element.getByRole('button', { name: 'Reset', exact: true }).click();
  }
  base() {
    return this.element.getByRole('group', { name: 'Base folder', exact: true });
  }
  counts() {
    return this.element.locator('footer');
  }
  async names() {
    return this.element.getByRole('option').locator('[data-name]').allTextContents();
  }
  async focus() {
    await this.body.click({ position: { x: 4, y: 4 } });
  }
  async selectAll() {
    await this.focus();
    await this.page.keyboard.press('ControlOrMeta+A');
  }
  async clear() {
    await this.page.keyboard.press('Escape');
  }
  async scrollToEnd() {
    await this.body.evaluate((element) => {
      element.scrollTop = element.scrollHeight;
    });
  }
  async scrollTo(top: number) {
    await this.body.evaluate((element, value) => {
      element.scrollTop = value;
    }, top);
  }
  async scrollTop() {
    return this.body.evaluate((element) => element.scrollTop);
  }
  async goto(appUrl: string, file: FixtureFile) {
    const key = this.side === 'source' ? 'src' : 'cmp';
    const url = new URL('/browse', appUrl);
    const dir = file.path.includes('/') ? file.path.slice(0, file.path.lastIndexOf('/')) : '';
    url.searchParams.set(key, `${file.volume}${dir ? `/${dir}` : ''}`);
    await this.page.goto(url.href);
    await expect(this.card(file.path.split('/').at(-1) ?? file.path).element).toBeVisible();
  }
}
