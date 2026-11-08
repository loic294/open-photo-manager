import { ok } from '../http.ts';
import { publicRoute, type Route, route } from '../router.ts';
import type { User } from '../types.ts';
import { iso } from '../util.ts';

/** `UserAdminResponseDto`: every field the spec requires, with plain values. */
export function userView(user: User) {
  return {
    id: user.id,
    email: user.email,
    name: user.name,
    isAdmin: user.isAdmin,
    avatarColor: 'primary',
    clusterGroupId: user.id,
    createdAt: iso(user.createdAt),
    updatedAt: iso(user.createdAt),
    deletedAt: null,
    license: null,
    oauthId: '',
    profileChangedAt: iso(user.createdAt),
    profileImagePath: '',
    quotaSizeInBytes: null,
    quotaUsageInBytes: 0,
    shouldChangePassword: false,
    status: 'active',
    storageLabel: user.isAdmin ? 'admin' : null,
  };
}

/** The number after `-rc.` (or any `-label.`) of `v3.4.0-rc.0`; null for a release. */
const prereleaseOf = (text: string): number | null => {
  const match = /-[A-Za-z]+\.(\d+)$/.exec(text);
  return match?.[1] === undefined ? null : Number(match[1]);
};

export const serverRoutes: Route[] = [
  publicRoute('GET', '/server/ping', () => ok({ res: 'pong' })),

  publicRoute('GET', '/server/version', ({ ctx }) => {
    const { major, minor, patch, text } = ctx.version;
    return ok({ major, minor, patch, featureLevel: 1, prerelease: prereleaseOf(text) });
  }),

  route('GET', '/server/about', { permission: 'server.about' }, ({ ctx }) =>
    ok({
      version: ctx.version.text,
      versionUrl: `https://github.com/immich-app/immich/releases/tag/${ctx.version.text}`,
      licensed: false,
    }),
  ),

  route('GET', '/users/me', { permission: 'user.read' }, ({ user }) => ok(userView(user))),
];
