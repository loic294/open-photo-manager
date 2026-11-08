import type { IncomingMessage } from 'node:http';
import { ApiError } from './http.ts';
import type { MockContext } from './mock.ts';
import { keyAllows } from './permissions.ts';
import type { LogEntry } from './requestlog.ts';
import type { ApiKey, Session, User } from './types.ts';
import { sha256Hex } from './util.ts';

/**
 * Who may call a route. `'none'`: anybody (login, ping). `'session'`: a login session only
 * (logout). Otherwise an API key that carries `permission`, or, when `session` is set, a login
 * session: the mock lets a session through only on the routes the app really calls with one.
 */
export type AuthSpec =
  | 'none'
  | 'session'
  | { permission: string; admin?: boolean; session?: boolean };

export interface Principal {
  user: User;
  key: ApiKey | null;
  session: Session | null;
}

const header = (req: IncomingMessage, name: string): string | undefined => {
  const value = req.headers[name];
  const text = Array.isArray(value) ? value[0] : value;
  return text === undefined || text === '' ? undefined : text;
};

const bearerToken = (req: IncomingMessage): string | undefined => {
  const match = /^Bearer\s+(\S+)$/i.exec(header(req, 'authorization') ?? '');
  return match?.[1];
};

function requireUser(ctx: MockContext, userId: string, message: string): User {
  const user = ctx.data.users.get(userId);
  if (user === undefined) {
    throw new ApiError(401, message);
  }
  return user;
}

/**
 * Identifies the caller and checks the route's requirements, in Immich's order: credentials
 * (401), then administrator (403), then permission (403). `entry` receives the kind of
 * credential for the request log.
 */
export function authenticate(
  ctx: MockContext,
  req: IncomingMessage,
  spec: AuthSpec,
  entry: LogEntry,
): Principal | null {
  if (spec === 'none') {
    return null;
  }
  const secret = header(req, 'x-api-key');
  const token = bearerToken(req);

  if (secret !== undefined) {
    const hash = sha256Hex(secret);
    const key = [...ctx.data.keys.values()].find((candidate) => candidate.secretHash === hash);
    if (key === undefined) {
      entry.auth = 'invalid';
      throw new ApiError(401, 'Invalid API key');
    }
    entry.auth = 'apiKey';
    entry.keyName = key.name;
    const user = requireUser(ctx, key.userId, 'Invalid API key');
    if (spec === 'session') {
      throw new ApiError(403, 'This route needs a login session: an API key is not accepted');
    }
    if (spec.admin && !user.isAdmin) {
      throw new ApiError(403, 'Forbidden');
    }
    if (!keyAllows(key, spec.permission)) {
      throw new ApiError(403, `Missing required permission: ${spec.permission}`);
    }
    return { user, key, session: null };
  }

  if (token !== undefined) {
    const session = ctx.data.sessions.get(token);
    if (session === undefined) {
      entry.auth = 'invalid';
      throw new ApiError(401, 'Invalid user token');
    }
    entry.auth = 'session';
    const user = requireUser(ctx, session.userId, 'Invalid user token');
    if (spec !== 'session' && !spec.session) {
      throw new ApiError(
        401,
        'The mock accepts a login session only on POST /api-keys, DELETE /api-keys/{id} and POST /auth/logout: use the API key here',
      );
    }
    if (spec !== 'session' && spec.admin && !user.isAdmin) {
      throw new ApiError(403, 'Forbidden');
    }
    return { user, key: null, session };
  }

  throw new ApiError(401, 'Authentication required');
}
