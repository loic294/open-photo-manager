import type { Page } from '@playwright/test';

export class PageGuard {
  readonly errors: string[] = [];
  constructor(page: Page, allowed: (string | RegExp)[]) {
    const record = (entry: string) => {
      if (
        !allowed.some((pattern) =>
          typeof pattern === 'string' ? entry.includes(pattern) : pattern.test(entry),
        )
      )
        this.errors.push(entry);
    };
    page.on('pageerror', (error) => record(`pageerror: ${error.message}`));
    page.on('console', (message) => {
      if (message.type() === 'error' || message.type() === 'warning')
        record(`console.${message.type()}: ${message.text()}`);
    });
    page.on('requestfailed', (request) =>
      record(`requestfailed: ${request.url()} ${request.failure()?.errorText}`),
    );
    page.on('response', (response) => {
      if (new URL(response.url()).pathname.startsWith('/api/') && response.status() >= 400)
        record(`API ${response.status()}: ${response.url()}`);
    });
  }
  assertClean() {
    if (this.errors.length)
      throw new Error(`Unexpected browser errors:\n${this.errors.join('\n')}`);
  }
}
