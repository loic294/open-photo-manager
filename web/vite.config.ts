import { fileURLToPath } from 'node:url';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

// Parallel worktrees each run their own stack: point the proxy at that backend's `OPM_LISTEN`.
const apiTarget = process.env.OPM_API_TARGET ?? 'http://127.0.0.1:8080';

export default defineConfig({
  plugins: [tailwindcss()],
  resolve: {
    alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
  },
  build: {
    target: 'es2022',
    outDir: 'dist',
  },
  server: {
    port: 5173,
    proxy: {
      '/api': {
        target: apiTarget,
        // The backend's CSRF check compares Origin to Host, so Host must stay as the browser sent it.
        changeOrigin: false,
        configure: (proxy) => {
          // The proxy only forwards SSE headers together with the first body chunk. Flush them
          // right away so EventSource opens before the first event. The microtask lets the proxy
          // copy the upstream headers onto the response first.
          proxy.on('proxyRes', (proxyRes, _req, res) => {
            if (proxyRes.headers['content-type']?.includes('text/event-stream')) {
              queueMicrotask(() => res.flushHeaders());
            }
          });
        },
      },
    },
  },
});
