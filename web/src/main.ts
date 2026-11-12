import './styles.css';

// The router matches paths with URLPattern, which browsers older than 2025 lack.
if (!('URLPattern' in globalThis)) await import('urlpattern-polyfill');

await import('@/app/opm-app');
