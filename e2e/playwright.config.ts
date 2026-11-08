import { cpus } from 'node:os';
import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './specs',
  fullyParallel: true,
  workers: Number(process.env.E2E_WORKERS ?? Math.max(1, Math.min(4, cpus().length / 2))),
  retries: 0,
  timeout: 30_000,
  expect: { timeout: 5_000 },
  globalSetup: './src/global-setup.ts',
  reporter: [['list'], ['html', { open: 'never' }]],
  use: {
    browserName: 'chromium',
    channel: process.env.E2E_CHANNEL,
    locale: 'en-US',
    timezoneId: 'UTC',
    colorScheme: 'light',
    contextOptions: { reducedMotion: 'reduce' },
    viewport: { width: 1440, height: 900 },
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
  },
});
