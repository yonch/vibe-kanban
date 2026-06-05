#!/usr/bin/env bash
set -euo pipefail

# Cargo's build.build-dir is configured as target/build-dir. In Vibe Kanban
# workspaces, scripts/vibe-kanban-setup.sh may replace that path with a symlink
# to a machine-local shared cache.
VK_REPO_BUILD_DIR="${VK_REPO_BUILD_DIR:-target/build-dir}"

if [ -e "$VK_REPO_BUILD_DIR" ] || [ -L "$VK_REPO_BUILD_DIR" ]; then
  find -H "$VK_REPO_BUILD_DIR" -type f -mtime +7 -delete 2>/dev/null || true
  find -H "$VK_REPO_BUILD_DIR" -mindepth 1 -type d -empty -delete 2>/dev/null || true
fi
