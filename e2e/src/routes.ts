import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { root } from './paths';

export interface ApiRoute {
  method: string;
  path: string;
  body?: object;
  mutates: boolean;
}

/** The server's independently maintained contract inventory is authoritative, not plan 5.12. */
export function apiRoutes(): ApiRoute[] {
  const source = readFileSync(join(root, 'crates/opm-server/tests/api_routes.rs'), 'utf8');
  const table = /const ROUTES:[\s\S]*?=\s*&\[([\s\S]*?)\];/.exec(source)?.[1];
  if (!table) throw new Error('Rust API contract ROUTES table was not found');
  const reads = new Set([
    '/api/browse/folders',
    '/api/browse/items',
    '/api/browse/ids',
    '/api/facets',
    '/api/format/preview',
    '/api/rename/preview',
  ]);
  const routes = [...table.matchAll(/\("([A-Z]+)",\s*"([^"]+)"\)/g)].map(([, method, path]) => ({
    method,
    path,
    body: ['POST', 'PUT'].includes(method) ? {} : undefined,
    mutates: !['GET', 'HEAD'].includes(method) && !reads.has(path),
  }));
  if (routes.length === 0) throw new Error('Rust API contract inventory is empty');
  return routes;
}

/** Parse actual router declarations as a separate oracle; adding a route must update ROUTES. */
export function implementedRoutes(): string[] {
  const directory = join(root, 'crates/opm-server/src/api');
  const module = readFileSync(join(directory, 'mod.rs'), 'utf8');
  const routes: string[] = [];
  for (const [, name] of module.matchAll(/\.merge\((\w+)::routes\(\)\)/g)) {
    const file = join(directory, `${name}.rs`);
    const source = readFileSync(existsSync(file) ? file : join(directory, name, 'mod.rs'), 'utf8');
    for (const [, path, methods] of source.matchAll(
      /\.route\(\s*"([^"]+)"\s*,([\s\S]*?)(?=\n\s*\.route|\n\s*\})/g,
    )) {
      for (const [, method] of methods.matchAll(/\b(get|post|put|delete|patch|head|options)\(/g)) {
        routes.push(`${method.toUpperCase()} /api${path.replace(/\{[^}]+\}/g, '1')}`);
      }
    }
  }
  return routes.sort();
}
