import { randomUUID } from 'node:crypto';
import { ApiError, badRequest, created, noContent, ok } from '../http.ts';
import { knownPermissions } from '../permissions.ts';
import { publicRoute, type Route, route } from '../router.ts';
import type { ApiKey } from '../types.ts';
import { iso, randomToken, sha256Hex } from '../util.ts';
import { createChecker } from '../validate.ts';

const EMAIL_RE = /^[^\s@]+@[^\s@]+\.[^\s@]{2,}$/;

const keyView = (key: ApiKey) => ({
  id: key.id,
  name: key.name,
  permissions: key.permissions,
  createdAt: iso(key.createdAt),
  updatedAt: iso(key.updatedAt),
});

export const authRoutes: Route[] = [
  publicRoute('POST', '/auth/login', ({ ctx, body }) => {
    const c = createChecker(body);
    c.only(['email', 'password']);
    const email = c.string('email', { required: true });
    const password = c.string('password', { required: true, nonEmpty: true });
    if (email !== undefined && !EMAIL_RE.test(email)) {
      c.fail('email', 'must be an email');
    }
    c.done();
    const user = [...ctx.data.users.values()].find(
      (candidate) =>
        candidate.email.toLowerCase() === email?.toLowerCase() && candidate.password === password,
    );
    if (user === undefined) {
      throw new ApiError(401, 'Incorrect email or password');
    }
    const token = randomToken(32);
    ctx.data.sessions.set(token, { token, userId: user.id, createdAt: ctx.clock.now() });
    return created({
      accessToken: token,
      userId: user.id,
      userEmail: user.email,
      name: user.name,
      isAdmin: user.isAdmin,
      isOnboarded: true,
      profileImagePath: '',
      shouldChangePassword: false,
    });
  }),

  route('POST', '/auth/logout', 'session', ({ ctx, session }) => {
    if (session !== null) {
      ctx.data.sessions.delete(session.token);
    }
    return ok({ successful: true, redirectUri: '/auth/login?autoLaunch=0' });
  }),

  route(
    'POST',
    '/api-keys',
    { permission: 'apiKey.create', session: true },
    ({ ctx, user, body }) => {
      const c = createChecker(body);
      c.only(['name', 'permissions']);
      const name = c.string('name');
      const permissions = c.oneOfList('permissions', knownPermissions());
      if (!c.has('permissions')) {
        c.fail('permissions', 'must be an array');
      } else if (permissions?.length === 0) {
        c.fail('permissions', 'must contain at least 1 elements');
      }
      c.done();
      const secret = randomToken(32);
      const now = ctx.clock.now();
      const key: ApiKey = {
        id: randomUUID(),
        name: name ?? 'API Key',
        permissions: permissions ?? [],
        secretHash: sha256Hex(secret),
        userId: user.id,
        createdAt: now,
        updatedAt: now,
      };
      ctx.data.keys.set(key.id, key);
      // The deprecated `apiKey` member stays next to the flattened fields, like the 3.4 spec says.
      return created({ ...keyView(key), secret, apiKey: keyView(key) });
    },
  ),

  route(
    'DELETE',
    '/api-keys/:id',
    { permission: 'apiKey.delete', session: true },
    ({ ctx, user, params }) => {
      const key = ctx.data.keys.get(params.id ?? '');
      if (key === undefined || key.userId !== user.id) {
        throw badRequest('API Key not found');
      }
      ctx.data.keys.delete(key.id);
      return noContent();
    },
  ),
];
