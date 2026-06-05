#!/usr/bin/env bash
set -euo pipefail

# Enable the repo pre-commit hook in each Vibe Kanban worktree.
git config core.hooksPath .githooks

# Share Cargo's repo-local build-dir across Vibe Kanban worktrees on this
# machine. The checked-in .cargo/config.toml falls back to target/build-dir
# for users without this setup script.
VK_SHARED_BUILD_DIR="${VK_SHARED_BUILD_DIR:-${CARGO_HOME:-$HOME/.cargo}/build-dir/vibe-kanban}"
VK_REPO_BUILD_DIR="${VK_REPO_BUILD_DIR:-target/build-dir}"
mkdir -p "$VK_SHARED_BUILD_DIR" target

if [ -L "$VK_REPO_BUILD_DIR" ]; then
  ln -sfn "$VK_SHARED_BUILD_DIR" "$VK_REPO_BUILD_DIR"
elif [ -e "$VK_REPO_BUILD_DIR" ]; then
  if rmdir "$VK_REPO_BUILD_DIR" 2>/dev/null; then
    ln -s "$VK_SHARED_BUILD_DIR" "$VK_REPO_BUILD_DIR"
  else
    echo "Leaving existing non-empty $VK_REPO_BUILD_DIR in place; shared Cargo build-dir disabled for this worktree."
  fi
else
  ln -s "$VK_SHARED_BUILD_DIR" "$VK_REPO_BUILD_DIR"
fi

scripts/vibe-kanban-cleanup.sh

# Install Node deps so prettier/tsc/eslint are available for `pnpm run format`,
# `pnpm run check`, and the pre-commit hook's `format:check` step.
pnpm install
