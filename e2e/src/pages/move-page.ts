import { expect, type Page, type Response } from '@playwright/test';
import type { BrowseRequest } from '../../../web/src/api/generated/BrowseRequest';
import type { FoldersResponse } from '../../../web/src/api/generated/FoldersResponse';
import type { ItemsResponse } from '../../../web/src/api/generated/ItemsResponse';
import type { MoveParams } from '../../../web/src/api/generated/MoveParams';
import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import { FileCard } from './pane';

export class MovePage {
  constructor(readonly page: Page) {}

  async previewAfter(action: () => Promise<unknown>) {
    const browses: Response[] = [];
    const observe = (response: Response) => {
      if (
        response.request().method() === 'POST' &&
        /^\/api\/browse\/(folders|items)$/.test(new URL(response.url()).pathname)
      )
        browses.push(response);
    };
    this.page.on('response', observe);
    const built = this.page.waitForResponse(
      (response) =>
        response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/plans',
    );
    try {
      await action();
      const response = await built;
      expect(response.status()).toBe(201);
      expect(await response.finished()).toBeNull();
      const plan = (await response.json()) as PlanDto;
      const { params } = response.request().postDataJSON() as { params: MoveParams };
      for (const [side, scope, heading] of [
        ['source', params.source.scope, 'Source'],
        ['comparison', params.destination, 'Destination'],
      ] as const) {
        const pane = this.page.getByRole('region', { name: heading, exact: true });
        for (const endpoint of ['folders', 'items'] as const) {
          const matches = (candidate: Response) => {
            if (new URL(candidate.url()).pathname !== `/api/browse/${endpoint}`) return false;
            const request = candidate.request().postDataJSON() as BrowseRequest & {
              offset?: number;
            };
            return (
              request.plan?.plan_id === plan.id &&
              request.plan.side === side &&
              request.volume_id === scope.volume_id &&
              request.dir === scope.dir &&
              (endpoint === 'folders' || request.offset === 0)
            );
          };
          await expect.poll(() => browses.some(matches)).toBe(true);
          const browse = browses.find(matches);
          if (!browse) throw new Error(`Missing ${side} ${endpoint} for plan ${plan.id}`);
          expect(browse.status()).toBe(200);
          expect(await browse.finished()).toBeNull();
          const body = (await browse.json()) as FoldersResponse | ItemsResponse;
          if ('items' in body) {
            expect(body.offset).toBe(0);
            const first = body.items[0];
            if (first) {
              await expect(pane.getByRole('listbox')).not.toHaveAttribute('aria-busy', 'true');
              const card = new FileCard(pane, first.name);
              await expect(card.element).toBeVisible();
              await expect(card.isGhost()).resolves.toBe(first.overlay.planned);
            } else {
              expect(body.total).toBe(0);
              await expect(pane.locator('apm-file-grid').getByRole('status')).toBeVisible();
              await expect(pane.getByRole('listbox')).toHaveCount(0);
            }
          } else {
            const first = body.folders[0];
            if (first) {
              const folder = pane.locator('apm-folder-list a[data-name]').filter({
                has: this.page.getByText(first.name, { exact: true }),
              });
              await expect(folder).toBeVisible();
              await expect
                .poll(() => folder.evaluate((row) => row.classList.contains('border-dashed')))
                .toBe(first.overlay.planned);
            }
          }
        }
        await expect(pane.locator('[data-pane-body]')).not.toHaveAttribute('aria-busy', 'true');
        await expect
          .poll(() =>
            pane.evaluate((element) => {
              const host = element.closest('apm-pane') as HTMLElement & {
                browseRequest: BrowseRequest | null;
              };
              return host.browseRequest;
            }),
          )
          .toMatchObject({
            volume_id: scope.volume_id,
            dir: scope.dir,
            plan: { plan_id: plan.id, side },
          });
      }
      await expect(this.actionButton()).toBeEnabled();
      return plan;
    } finally {
      this.page.off('response', observe);
    }
  }

  moveType() {
    return this.page.getByRole('combobox', { name: 'Move type', exact: true });
  }

  async chooseMoveType(value: 'keep_source' | 'keep_destination' | 'flat' | 'custom') {
    await this.moveType().selectOption(value);
  }

  matchMode() {
    return this.page.getByRole('combobox', { name: 'Match by', exact: true });
  }

  policy() {
    return this.page.getByRole('combobox', {
      name: 'If a file with the same name exists',
      exact: true,
    });
  }

  async choosePolicy(value: 'default' | 'smart' | 'skip' | 'rename' | 'replace') {
    await this.policy().selectOption(value);
  }

  pattern() {
    return this.page.getByRole('textbox', { name: 'Folder pattern', exact: true });
  }

  async openPattern() {
    await this.page
      .locator('details.collapse > summary')
      .filter({ hasText: 'Folder pattern:' })
      .click();
  }

  actionButton() {
    return this.page.getByRole('button', { name: /^Move files/ });
  }

  planNotices() {
    return this.page
      .getByRole('status')
      .filter({ hasText: /Choose the (source|destination) folder/ });
  }

  sourceBase() {
    return this.page.getByRole('region', { name: 'Source' }).getByRole('group', {
      name: 'Base folder',
    });
  }

  destinationBase() {
    return this.destinationPane().getByRole('group', {
      name: 'Base folder',
    });
  }

  destinationPane() {
    return this.page.getByRole('region', { name: 'Destination', exact: true });
  }

  destinationCard(name: string) {
    return new FileCard(this.destinationPane(), name);
  }

  destinationFolder(name: string) {
    return this.destinationPane()
      .locator('apm-folder-list a[data-name]')
      .filter({ has: this.page.getByText(name, { exact: true }) });
  }

  async openDestinationFolder(name: string) {
    await this.destinationFolder(name).click();
    await expect(this.destinationPane().locator('[data-pane-body]')).not.toHaveAttribute(
      'aria-busy',
      'true',
    );
  }
}
