import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

export const root = fileURLToPath(new URL('../../', import.meta.url));
export const cache = join(root, 'e2e/.cache');
export const golden = join(cache, 'golden');
export const binary = join(root, `target/${process.env.E2E_PROFILE ?? 'debug'}/apm-server`);
