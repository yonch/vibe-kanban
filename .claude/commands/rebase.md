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

- `auto/2d45-the-vibe-kanban` — GHCR build workflow (yonch-specific CI)
- `auto/cd36-latency-launchin` — Latency instrumentation for executor launch path
- `vk/rebase-skill` — /rebase skill for maintaining fork on top of upstream

## Procedure

Execute these steps in order:

### 1. Fetch latest upstream

```
git fetch upstream main
git fetch origin
```

### 2. Identify commits on origin/main not on upstream/main

```
git log --oneline origin/main --not upstream/main
```

### 3. Find open upstream PRs by yonch

```
gh api "search/issues?q=author:yonch+repo:BloopAI/vibe-kanban+type:pr+state:open&per_page=50" \
  --jq '.items[] | "\(.number)\t\(.title)\t\(.html_url)"'
```

### 4. Match commits to branches

For each commit from step 2, determine which branch it belongs to:
- Check fork-only branches listed above
- Check open upstream PR branches (the PR head ref names)
- Match by commit message, author, or by checking `git branch -r --contains <sha>`

### 5. Handle unmatched commits

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

### 6. Rebase each branch onto upstream/main

For every branch (fork-only and PR branches):

```
git checkout <branch>
git rebase upstream/main
```

If conflicts occur, resolve them:
- `Cargo.lock`: accept upstream's, then `cargo generate-lockfile`
- API renames: update to upstream's current names (check imports/exports)
- Method renames: update to upstream's current signatures

### 7. Push all rebased branches

```
git push origin <branch1> <branch2> ... --force-with-lease
```

### 8. Rebuild origin/main

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

### 9. Push rebuilt main

```
git push origin main --force-with-lease
```

### 10. Verify

```
git log --oneline main --not upstream/main
```

Confirm the commit count matches expectations (sum of all branch commits, no fixup/maintenance commits).

## When upstream merges a PR

If a PR branch has been merged upstream, its commits will already be in `upstream/main`.
Simply remove it from the cherry-pick sequence in step 8. The branch can be deleted:

```
git push origin --delete <branch-name>
git branch -d <branch-name>
```

## Printing a summary table

After completing the rebase, print a summary table:

| # | Feature | Branch | Upstream PR | Type | Commits |
|---|---------|--------|-------------|------|---------|

With type being "Fork-only" or "Open PR" and a link to the upstream PR if applicable.
