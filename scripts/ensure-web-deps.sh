#!/usr/bin/env bash
# Make web/node_modules match web/package-lock.json of the current checkout.
# node_modules is cloned from another checkout (wt-new.sh), so a dependency change that landed later
# leaves a worktree with a stale install ("Cannot find module ..."). Reinstalls only when the lockfile
# differs from the one the stamp inside node_modules was written for. Used by wt-new.sh and wt-land.sh.
set -euo pipefail

cd "$(dirname "$0")/.."
stamp=web/node_modules/.opm-lock-sha
want="$(shasum web/package-lock.json | cut -d' ' -f1)"

if [ -d web/node_modules ] && [ "$(cat "$stamp" 2>/dev/null || true)" = "$want" ]; then
  exit 0
fi

echo ">> web dependencies are out of date for this checkout: npm ci"
npm --prefix web ci --no-audit --no-fund --prefer-offline
echo "$want" >"$stamp"
