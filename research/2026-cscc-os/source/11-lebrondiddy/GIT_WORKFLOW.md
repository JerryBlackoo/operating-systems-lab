# Git Workflow

This project must keep a real commit history because the contest requires
traceable development, meaningful commit notes, and AI usage disclosure.

## Repository

Remote:

```text
origin https://T2026104229910571@gitlab.eduxiji.net/T2026104229910571/oskernel2025-x.git
```

The explicit username is intentional. The user has multiple EduXiji/GitLab
competition accounts, and this URL keeps this repository from accidentally
using another cached account.

## Normal Loop

After every accepted change:

```sh
git status --short --branch
git add <changed-files>
git commit -m "TASK-XXX short summary" \
  -m "AI-Usage: <agent/model> did <brief work> under user direction" \
  -m "Task: TASK-XXX"
git push
```

Use meaningful commits. Do not batch unrelated work into one commit.

## If Push Says `fetch first`

This means remote `main` has commits that local `main` does not have.

First inspect:

```sh
git fetch origin main
git log --oneline --decorate --graph --max-count=12 --all
git diff --stat main..origin/main
git diff --name-status main..origin/main
```

If the remote changes are safe to keep, merge them:

```sh
git merge origin/main
git push
```

If local and remote were created independently, Git may require:

```sh
git merge origin/main --allow-unrelated-histories --no-edit
git push
```

Do not force push unless the user explicitly approves it after seeing what will
be overwritten.

## If Push Says 403

Check which account Git is using without printing tokens:

```sh
printf 'protocol=https\nhost=gitlab.eduxiji.net\n\n' \
  | git credential fill \
  | sed -n 's/^username=/username=/p'
```

If the username is not `T2026104229910571`, keep both accounts by using an
explicit-username remote URL:

```sh
git remote set-url origin \
  https://T2026104229910571@gitlab.eduxiji.net/T2026104229910571/oskernel2025-x.git
git push
```

When prompted for a password, use a Personal Access Token for
`T2026104229910571` with at least `write_repository`.

If the username is correct but push still returns 403, check GitLab project
membership and protected branch settings:

- The pushing account must be a project member.
- Permission should be at least Developer; Maintainer is safer for protected
  `main`.
- Protected branch settings must allow the account/role to push to `main`.

## Current Publication State

Remote initially contained `54e38bd Initial commit` with `README.md`. Local
workflow history was merged with it using `--allow-unrelated-histories`, then
pushed successfully. Current `main` tracks `origin/main`.

