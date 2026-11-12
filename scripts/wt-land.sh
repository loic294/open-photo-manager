#!/usr/bin/env bash
# Land the current feature worktree on main through a serialized queue:
#   wait for the lock -> rebase onto main -> run the gates for what changed -> fast-forward main.
# Run from inside a feat/* worktree with all work committed. Prints "LANDED: ..." on success.
# Exit codes: 2 usage or uncommitted changes, 3 rebase conflict, 4 gate failed or gates dirtied
# the tree, 5 main checkout dirty, 6 timed out waiting for the lock or fast-forward failed.
set -uo pipefail

cd "$(git rev-parse --show-toplevel)" || exit 2
MAIN="$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")"
BR="$(git rev-parse --abbrev-ref HEAD)"
LOCK="${OPM_LAND_LOCK:-/tmp/opm-land.lock}"
WAIT_MAX="${OPM_LAND_WAIT_SECS:-2700}"

if [ "$BR" = "main" ] || [ "$BR" = "HEAD" ]; then
  echo "ERROR: run this inside a feat/* worktree, not on main"
  exit 2
fi
if [ -n "$(git status --porcelain)" ]; then
  echo "ERROR: uncommitted changes. Run: git add -A && git commit"
  exit 2
fi

trap 'exit 130' INT TERM HUP
waited=0
while ! mkdir "$LOCK" 2>/dev/null; do
  owner="$(cat "$LOCK/pid" 2>/dev/null || true)"
  if [ -n "$owner" ] && ! kill -0 "$owner" 2>/dev/null; then
    echo ">> removing stale landing lock (pid $owner is gone)"
    rm -rf "$LOCK"
    continue
  fi
  if [ -z "$owner" ] && [ -n "$(find "$LOCK" -maxdepth 0 -mmin +2 2>/dev/null)" ]; then
    rm -rf "$LOCK"
    continue
  fi
  if [ "$waited" -ge "$WAIT_MAX" ]; then
    echo "ERROR: waited ${WAIT_MAX}s for the landing lock; re-run scripts/wt-land.sh"
    exit 6
  fi
  if [ $((waited % 60)) -eq 0 ]; then
    echo ">> queued: waiting for $(cat "$LOCK/branch" 2>/dev/null || echo another agent) to finish landing (${waited}s)"
  fi
  sleep 10
  waited=$((waited + 10))
done
echo $$ >"$LOCK/pid"
echo "$BR" >"$LOCK/branch"
trap 'rm -rf "$LOCK"' EXIT

echo ">> rebasing $BR onto main"
if ! git rebase main; then
  cat <<'EOF'
REBASE_CONFLICT: resolve the conflicts, 'git add' the files, 'git rebase --continue', then re-run scripts/wt-land.sh.
  - Shared wiring files (lib.rs, mod.rs, state.rs, Cargo.toml, ...): keep BOTH sides' additions.
  - Cargo.lock: 'git checkout --ours Cargo.lock' (during a rebase "ours" is main), then run 'cargo check' to regenerate it.
  - web/package-lock.json: 'git checkout --ours web/package-lock.json', then 'npm --prefix web install'.
  - Generated TypeScript (web/src/api/generated): take either side, then run 'make gen-api' and add the result.
EOF
  exit 3
fi

CHANGED="$(git diff --name-only main..HEAD)"
if printf '%s\n' "$CHANGED" | grep -qE '^(crates/|Cargo\.|\.cargo/|rust-toolchain|rustfmt)'; then
  echo ">> gate: make check-rust"
  make check-rust || { echo "GATE_FAILED(rust): fix it, commit, and re-run scripts/wt-land.sh"; exit 4; }
fi
if printf '%s\n' "$CHANGED" | grep -qE '^(web/|crates/opm-core/src/dto/)'; then
  echo ">> gate: make check-web"
  scripts/ensure-web-deps.sh || { echo "GATE_FAILED(web): 'npm ci' failed, fix the cause and re-run scripts/wt-land.sh"; exit 4; }
  make check-web || { echo "GATE_FAILED(web): fix it, commit, and re-run scripts/wt-land.sh"; exit 4; }
fi
if printf '%s\n' "$CHANGED" | grep -qE '^(tools/|e2e/)'; then
  echo ">> gate: make check-tools"
  make check-tools || { echo "GATE_FAILED(tools): fix it, commit, and re-run scripts/wt-land.sh"; exit 4; }
fi
if [ -n "$(git status --porcelain)" ]; then
  echo "GATE_DIRTY: the gates modified files (regenerated TypeScript types? formatting?)."
  echo "  Review 'git status', commit the changes (git add -A && git commit) and re-run scripts/wt-land.sh."
  git status --short
  exit 4
fi

for i in 1 2 3 4 5 6 7 8 9 10 11 12; do
  if [ -z "$(git -C "$MAIN" status --porcelain)" ]; then
    break
  fi
  if [ "$i" -eq 12 ]; then
    echo "ERROR: the main checkout ($MAIN) has uncommitted changes. Do NOT touch it; tell the coordinator."
    git -C "$MAIN" status --short
    exit 5
  fi
  sleep 10
done

if ! git merge-base --is-ancestor main HEAD; then
  echo "ERROR: main moved while the landing lock was held (a land-script bypass?). Re-run scripts/wt-land.sh."
  exit 6
fi
if git -C "$MAIN" merge --ff-only "$BR" >/dev/null; then
  echo "LANDED: $BR -> main ($(git rev-parse --short HEAD))"
  exit 0
fi
echo "ERROR: fast-forward of main failed. Re-run scripts/wt-land.sh."
exit 6
