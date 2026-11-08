import type { Route } from '../router.ts';
import { albumRoutes, tagRoutes } from './albums-tags.ts';
import { assetRoutes } from './assets.ts';
import { authRoutes } from './auth.ts';
import { libraryRoutes } from './libraries.ts';
import { searchRoutes } from './search.ts';
import { serverRoutes } from './server.ts';

/** Every call the Rust client makes, and nothing else. */
export const routes: Route[] = [
  ...serverRoutes,
  ...authRoutes,
  ...libraryRoutes,
  ...assetRoutes,
  ...searchRoutes,
  ...albumRoutes,
  ...tagRoutes,
];
