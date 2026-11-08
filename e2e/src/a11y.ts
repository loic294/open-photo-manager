import AxeBuilder from '@axe-core/playwright';
import { expect, type Page } from '@playwright/test';

export async function expectNoSeriousA11yViolations(
  page: Page,
  options: { include?: string[]; exclude?: string[] } = {},
) {
  let axe = new AxeBuilder({ page });
  for (const selector of options.include ?? []) axe = axe.include(selector);
  for (const selector of options.exclude ?? []) axe = axe.exclude(selector);
  const results = await axe.analyze();
  expect(
    results.violations.filter(
      (violation) => violation.impact === 'serious' || violation.impact === 'critical',
    ),
  ).toEqual([]);
}
