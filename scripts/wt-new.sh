#!/usr/bin/env bash
# Create isolated git worktrees (branch feat/<id>) from main for parallel work.
# Usage: scripts/wt-new.sh <todo-id> [<todo-id> ...]
# Worktrees live in <repo>-wt/<todo-id>. Rust target/ and web/node_modules are cloned
# with APFS copy-on-write (cp -c) so dependency caches are warm and cost no extra disk.
set -euo pipefail

MAIN="$(cd "$(dirname "$0")/.." && pwd)"
WT_ROOT="${OPM_WT_ROOT:-${MAIN}-wt}"
mkdir -p "$WT_ROOT"

# The clone below must carry an install that matches main's lockfile (cheap when it already does).
"$MAIN/scripts/ensure-web-deps.sh"

for id in "$@"; do
  dir="$WT_ROOT/$id"
  if [ -d "$dir" ]; then
    echo "exists: $dir"
    continue
  fi
  git -C "$MAIN" worktree add -q -b "feat/$id" "$dir" main
  if [ -d "$MAIN/target" ]; then
    cp -cRp "$MAIN/target" "$dir/target" 2>/dev/null || cp -Rp "$MAIN/target" "$dir/target"
  fi
  if [ -d "$MAIN/web/node_modules" ]; then
    cp -cRp "$MAIN/web/node_modules" "$dir/web/node_modules" 2>/dev/null || cp -Rp "$MAIN/web/node_modules" "$dir/web/node_modules"
  fi
  echo "ready: $dir (branch feat/$id)"
done
