import type { IncomingMessage } from 'node:http';
import { type AuthSpec, authenticate, type Principal } from './auth.ts';
import { ApiError, badRequest, type Reply } from './http.ts';
import type { MockContext } from './mock.ts';
import type { LogEntry } from './requestlog.ts';
import type { ApiKey, Session, User } from './types.ts';
import { isUuid } from './util.ts';

export type Params = Record<string, string>;

/** What a handler of a route that needs no credentials gets. */
export interface PublicInput {
  ctx: MockContext;
  params: Params;
  query: URLSearchParams;
  /** The parsed JSON body; `undefined` when there was none. */
  body: unknown;
}

/** What a handler of an authenticated route gets. */
export interface PrivateInput extends PublicInput {
  user: User;
  key: ApiKey | null;
  session: Session | null;
}

export interface Route {
  method: string;
  /** Relative to `/api`; `:name` stands for one path segment. */
  pattern: string;
  auth: AuthSpec;
  run(input: PublicInput, principal: Principal | null): Reply | Promise<Reply>;
}

type Handler<I> = (input: I) => Reply | Promise<Reply>;

export const publicRoute = (
  method: string,
  pattern: string,
  handle: Handler<PublicInput>,
): Route => ({
  method,
  pattern,
  auth: 'none',
  run: (input) => handle(input),
});

export const route = (
  method: string,
  pattern: string,
  auth: Exclude<AuthSpec, 'none'>,
  handle: Handler<PrivateInput>,
): Route => ({
  method,
  pattern,
  auth,
  run(input, principal) {
    if (principal === null) {
      throw new Error(`${method} ${pattern} ran without a principal`);
    }
    return handle({ ...input, ...principal });
  },
});

interface Compiled {
  route: Route;
  regex: RegExp;
  names: string[];
}

export interface Router {
  dispatch(
    ctx: MockContext,
    req: IncomingMessage,
    path: string,
    query: URLSearchParams,
    body: unknown,
    entry: LogEntry,
  ): Promise<Reply>;
}

function compile(route: Route): Compiled {
  const names: string[] = [];
  const source = route.pattern
    .split('/')
    .map((segment) => {
      if (segment.startsWith(':')) {
        names.push(segment.slice(1));
        return '([^/]+)';
      }
      return segment.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    })
    .join('/');
  return { route, regex: new RegExp(`^${source}$`), names };
}

/** A malformed escape stays as it is: the UUID check of the path ids then rejects it. */
function decode(segment: string): string {
  try {
    return decodeURIComponent(segment);
  } catch {
    return segment;
  }
}

/** Matches a request to a route and runs it: authentication, then path ids, then the handler. */
export function createRouter(routes: readonly Route[]): Router {
  const compiled = routes.map(compile);

  return {
    async dispatch(ctx, req, path, query, body, entry) {
      const method = req.method ?? 'GET';
      const relative = path.startsWith('/api/') ? path.slice('/api'.length) : null;
      let found: { compiled: Compiled; params: Params } | undefined;
      if (relative !== null) {
        const bare = relative.length > 1 ? relative.replace(/\/+$/, '') : relative;
        for (const candidate of compiled) {
          if (candidate.route.method !== method) {
            continue;
          }
          const match = candidate.regex.exec(bare);
          if (match !== null) {
            const params: Params = {};
            candidate.names.forEach((name, index) => {
              params[name] = decode(match[index + 1] ?? '');
            });
            found = { compiled: candidate, params };
            break;
          }
        }
      }
      if (found === undefined) {
        entry.unexpected = true;
        throw new ApiError(404, `Cannot ${method} ${path}`);
      }

      const principal = authenticate(ctx, req, found.compiled.route.auth, entry);
      for (const [name, value] of Object.entries(found.params)) {
        if (!isUuid(value)) {
          throw badRequest(`${name} must be a UUID`);
        }
      }
      return found.compiled.route.run({ ctx, params: found.params, query, body }, principal);
    },
  };
}
