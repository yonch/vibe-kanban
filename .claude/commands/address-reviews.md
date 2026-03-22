# Address BugBot review comments on open PRs

Find all open PRs by yonch on BloopAI/vibe-kanban, identify unresolved BugBot (Cursor) review comments, analyze them, and optionally fix them via workspaces.

## Context

This is a fork (`origin` = `yonch/vibe-kanban`) of `upstream` = `BloopAI/vibe-kanban`.
The upstream repo uses Cursor BugBot (`cursor[bot]`) for automated code reviews on PRs.

## Procedure

### 1. Find open PRs by yonch

```
gh api repos/BloopAI/vibe-kanban/pulls \
  --jq '.[] | select(.user.login == "yonch") | {number, title, head: .head.ref}'
```

### 2. Check each PR for unresolved BugBot review threads

Use the GitHub GraphQL API to get review thread resolution status:

```
gh api graphql -f query='
{
  repository(owner: "BloopAI", name: "vibe-kanban") {
    pullRequest(number: <N>) {
      reviewThreads(first: 20) {
        nodes {
          isResolved
          comments(first: 1) {
            nodes { bodyText, author { login } }
          }
        }
      }
    }
  }
}'
```

Filter for threads where `author.login == "cursor"` and `isResolved == false`.

Query multiple PRs in one GraphQL call using aliases (e.g. `pr3222: pullRequest(number: 3222) { ... }`).

### 3. Fetch detailed BugBot comments for PRs with unresolved threads

For each PR that has unresolved BugBot threads:

```
gh api repos/BloopAI/vibe-kanban/pulls/<N>/comments \
  --jq '.[] | select(.user.login == "cursor[bot]") | {id, path, line, body}'
```

### 4. Analyze each unresolved comment

For each unresolved BugBot comment:
- Read the referenced file(s) in the codebase to understand the actual code
- Assess whether the concern is **valid** or a false positive
- Rate the severity and practical risk
- Determine the fix if valid

### 5. Present summary to user

Show a summary table:

| PR | Issue | Severity | Valid? | Fix |
|---|---|---|---|---|
| #NNNN | Description | High/Medium/Low | Yes/No | Brief fix description |

### 6. Ask user which to fix

Ask the user which PRs/issues they want to fix. Options:
- Fix all
- Fix specific PRs
- Skip

### 7. Start workspaces to fix issues

For each PR the user wants fixed, start a workspace using the vibe-kanban MCP:

- **Repo**: `614d0600-d084-449c-af1c-5bdf9ba3d7a5` (vibe-kanban)
- **Branch**: The PR's head branch (e.g. `vk/wait-for-workspace`)
- **Executor**: `CLAUDE_CODE`
- **Prompt**: Include:
  - FYI what branch they're on and which PR it corresponds to
  - Each unresolved BugBot comment: the issue title, severity, file/lines, description, and recommended fix
  - Instruction to run `pnpm run format` then commit locally
  - **Do NOT push.** Tell the agent to commit but NOT push. Explain that the user will review first and approve before pushing.

Start all workspaces in parallel (one per PR).

### 8. Present workspace results for review

After workspaces complete, show the user what each workspace did (branch name, commit messages, files changed).

The user will review and either approve or request changes. When the user approves (e.g. "lgtm", "looks good", "approve", "push", "ship it"):
- For each approved workspace, push the fix to the corresponding PR branch:
  ```
  # Run inside the workspace via a new session prompt:
  git push origin HEAD:<pr-head-branch>
  ```
  Use the PR head branch name recorded from step 1 — the user should NOT need to specify which branch to push to.

If the user requests changes, start a new session in the workspace with the feedback and repeat.

### 9. Verify after push

After pushing, re-check the PRs using the GraphQL query from step 2 to verify:
- Previously unresolved threads are now resolved
- No new BugBot issues appeared on new commits

If new issues appeared, report them and offer to fix.
