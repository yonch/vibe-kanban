# Rebase fork on upstream

Rebuild `origin/main` as a clean linear stack on top of `upstream/main`.

## Context

This is a fork (`origin` = `yonch/vibe-kanban`) of `upstream` = `BloopAI/vibe-kanban`.
Our `origin/main` is composed of:
1. Everything on `upstream/main`
2. **Fork-only commits** that stay private (never submitted upstream)
3. **Upstream PR commits** from branches with open PRs on `BloopAI/vibe-kanban`

## Fork-only branches (permanent, never PR'd)

These branches contain commits that are private to this fork:

- `auto/cd36-latency-launchin` — Latency instrumentation for executor launch path
- `vk/rebase-skill` — /rebase skill for maintaining fork on top of upstream
- `vk/address-reviews` — /address-reviews skill for triaging BugBot PR comments
- `vk/conversation-race-conditions` — Fix conversation race conditions when switching workspaces
- `vk/local-build-gtk-fix` — Build only needed binaries to avoid GTK dependency

## Procedure

Execute these steps in order:

### 1. Fetch latest upstream

```
git fetch upstream main
git fetch origin
```

### 2. Create a backup branch

Push a backup of the current `origin/main` so we can recover if the rebase breaks:

```
git push origin origin/main:refs/heads/rebase-backup-$(date -u +%Y%m%d-%H%M) --no-verify
```

### 3. Identify commits on origin/main not on upstream/main

```
git log --oneline origin/main --not upstream/main
```

### 4. Find open upstream PRs by yonch

```
gh api "search/issues?q=author:yonch+repo:BloopAI/vibe-kanban+type:pr+state:open&per_page=50" \
  --jq '.items[] | "\(.number)\t\(.title)\t\(.html_url)"'
```

### 5. Match commits to branches

For each commit from step 3, determine which branch it belongs to:
- Check fork-only branches listed above
- Check open upstream PR branches (the PR head ref names)
- Match by commit message, author, or by checking `git branch -r --contains <sha>`

### 6. Handle unmatched commits

For any commit that does NOT belong to a known fork-only branch or upstream PR branch:

**Ask the user**: "Commit `<sha> <message>` is not associated with any PR or fork-only branch. Is this fork-only, or should it be submitted as an upstream PR?"

- **If fork-only**: Create a new branch from `upstream/main`, cherry-pick the commit(s), and add the branch name to the "Fork-only branches" list in this file.
- **If needs upstream PR**:
  1. Create a clean branch off `upstream/main`: `git checkout -b vk/<descriptive-name> upstream/main`
  2. Cherry-pick the commit(s) onto it
  3. Verify it compiles/builds if possible
  4. Push the branch: `git push origin vk/<descriptive-name>`
  5. Provide the user with:
     - **URL**: `https://github.com/BloopAI/vibe-kanban/compare/main...yonch:vibe-kanban:vk/<branch-name>?expand=1`
     - **Title**: a concise PR title
     - **Description**: a short summary of what the change does
  6. Wait for the user to confirm they opened the PR before continuing.

### 7. Rebase each branch onto upstream/main

For every branch (fork-only and PR branches):

```
git checkout <branch>
git rebase upstream/main
```

If conflicts occur, resolve them:
- `Cargo.lock`: accept upstream's, then `cargo generate-lockfile`
- API renames: update to upstream's current names (check imports/exports)
- Method renames: update to upstream's current signatures

After rebasing each branch (whether or not conflicts occurred), ensure `Cargo.lock` is consistent:

```
# If the branch modifies any Cargo.toml relative to upstream/main,
# re-resolve Cargo.lock to pick up new/changed dependencies.
if ! git diff --quiet upstream/main -- '**/Cargo.toml' 'Cargo.toml'; then
  git checkout upstream/main -- Cargo.lock
  # Find changed crates and update just those in the lockfile
  for toml in $(git diff --name-only upstream/main -- '**/Cargo.toml'); do
    crate_dir=$(dirname "$toml")
    crate_name=$(grep '^name' "$toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')
    cargo update -p "$crate_name" 2>/dev/null || true
  done
  if ! git diff --quiet -- Cargo.lock; then
    git add Cargo.lock
    git commit --amend --no-edit
  fi
fi
```

### 8. Push all rebased branches

```
git push origin <branch1> <branch2> ... --force-with-lease
```

### 9. Rebuild origin/main

Cherry-pick in this order: **fork-only first, then PR branches**.

```
git checkout -B main upstream/main

# Fork-only
git cherry-pick <fork-only-branch-1>~N..<fork-only-branch-1>
git cherry-pick <fork-only-branch-2>~N..<fork-only-branch-2>

# PR branches (in chronological order of PR number)
git cherry-pick <pr-branch-1>~N..<pr-branch-1>
git cherry-pick <pr-branch-2>~N..<pr-branch-2>
...
```

### 10. Verify locally

Before pushing, run the TypeScript type check to catch import/conflict resolution errors:

```
pnpm i && pnpm -C packages/web-core run check
```

If it fails, fix the issues (usually stale imports from cherry-pick conflict resolution), commit the fix on `main`, and re-check.

### 11. Push rebuilt main

```
git push origin main --force-with-lease
```

### 12. Verify remote builds

Pushing to `origin/main` automatically triggers Argo CI workflows via GitHub webhook:
- `vk-remote-image` — builds the remote (cloud) container from `crates/remote/Dockerfile`

Monitor the build:

```
# Wait for the workflow to appear (may take a few seconds after push)
kubectl get workflows -n dev --sort-by=.metadata.creationTimestamp | tail -5

# Watch logs of the latest vk-remote-image workflow
argo logs -n dev -f $(kubectl get workflows -n dev --sort-by=.metadata.creationTimestamp \
  -o jsonpath='{.items[-1].metadata.name}' -l workflows.argoproj.io/workflow-template=vk-remote-image)
```

If the remote build fails:
- Check logs: `argo logs -n dev <workflow-name>`
- Common causes: import conflicts from cherry-pick ordering, missing re-exports between branches
- Fix on `main`, re-push (this will trigger a new build automatically)

### 13. Verify commit count

```
git log --oneline main --not upstream/main
```

Confirm the commit count matches expectations (sum of all branch commits, no fixup/maintenance commits).

## When upstream merges a PR

If a PR branch has been merged upstream, its commits will already be in `upstream/main`.
Simply remove it from the cherry-pick sequence in step 9. The branch can be deleted:

```
git push origin --delete <branch-name>
git branch -d <branch-name>
```

## Printing a summary table

After completing the rebase, print a summary table:

| # | Feature | Branch | Upstream PR | Type | Commits |
|---|---------|--------|-------------|------|---------|

With type being "Fork-only" or "Open PR" and a link to the upstream PR if applicable.
