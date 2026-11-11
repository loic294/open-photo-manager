import type { Route } from '@playwright/test';
import type { BrowseRequest } from '../../../web/src/api/generated/BrowseRequest';
import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import { expect, test } from '../../src/test';

test('move setup waits for its real slow browse bodies before the next mode edit', async ({
  app,
  page,
  source,
  movePage,
}) => {
  let release: () => void = () => {};
  const released = new Promise<void>((resolve) => {
    release = resolve;
  });
  let announceHeld: () => void = () => {};
  const held = new Promise<void>((resolve) => {
    announceHeld = resolve;
  });
  const expected = new Set([
    'source/folders',
    'comparison/folders',
    'source/items',
    'comparison/items',
  ]);
  const heldKeys = new Set<string>();
  const deliveries: Promise<void>[] = [];
  let planId: number | undefined;
  const planResponse = (async () => {
    const response = await page.waitForResponse(
      (response) =>
        response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/plans',
    );
    const plan = (await response.json()) as PlanDto;
    planId = plan.id;
  })();
  const gate = async (route: Route) => {
    const request = route.request();
    const body = request.postDataJSON() as BrowseRequest;
    if (body.plan === null) return route.continue();
    await planResponse;
    if (body.plan.plan_id !== planId) return route.continue();
    const endpoint = new URL(request.url()).pathname.split('/').at(-1);
    const key = `${body.plan.side}/${endpoint}`;
    if (!expected.has(key)) return route.continue();
    const response = await route.fetch();
    expect(response.status()).toBe(200);
    const bytes = await response.body();
    expect(JSON.parse(bytes.toString())).toBeTruthy();
    heldKeys.add(key);
    if (heldKeys.size === expected.size) announceHeld();
    await released;
    await route.fulfill({ response, body: bytes });
  };
  await page.route(/\/api\/browse\/(?:folders|items)$/, (route) => {
    const delivery = gate(route);
    deliveries.push(delivery);
    return delivery;
  });
  let setup: Promise<unknown> | undefined;
  let setupComplete = false;
  try {
    await page.goto(`${app.url}/move?src=photos/dupes/m1&cmp=archive`);
    await source.useFolder();
    await expect(movePage.planNotices()).toContainText('Choose the destination folder');
    setup = movePage
      .previewAfter(() =>
        movePage
          .destinationBase()
          .getByRole('button', { name: 'Use this folder', exact: true })
          .click(),
      )
      .then(() => {
        setupComplete = true;
      });
    await held;
    expect(heldKeys).toEqual(expected);
    expect(setupComplete, 'setup must not return with its preview bodies withheld').toBe(false);
    await expect(movePage.moveType()).toHaveValue('keep_source');
    release();
    await setup;
    await movePage.previewAfter(() => movePage.chooseMoveType('custom'));
    await expect(movePage.moveType()).toHaveValue('custom');
    await expect(movePage.destinationFolder('2021')).toHaveClass(/border-dashed/);
  } finally {
    release();
    try {
      await setup;
    } finally {
      try {
        await Promise.all(deliveries);
      } finally {
        await page.unroute(/\/api\/browse\/(?:folders|items)$/);
      }
    }
  }
});
