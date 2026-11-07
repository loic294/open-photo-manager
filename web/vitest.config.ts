import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vitest/config';

export default defineConfig({
  resolve: {
    alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
  },
  test: {
    environment: 'happy-dom',
    include: ['src/**/*.test.ts'],
    // The on-disk transform cache ignores tsconfig.json changes (e.g. useDefineForClassFields)
    // and gives no speed-up across worktrees, so it is disabled.
    fsModuleCache: false,
  },
});
