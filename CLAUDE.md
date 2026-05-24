# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:ca08a54f -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

## Session Completion

**This repo is local-only — there is no git remote configured. Skip
the push step until a remote is added** (`git remote -v` is empty,
and bd memory `audit-2026-05-22-fitness` confirms "Issues are saved
locally only"). Do NOT spend cycles trying to push to a remote that
doesn't exist.

**MANDATORY WORKFLOW (local-only variant):**

1. **File issues for remaining work** — Create bd issues for anything
   that needs follow-up
2. **Run quality gates** (if code changed) — Tests, linters, builds
3. **Update issue status** — Close finished work, update in-progress
   items via `bd close <id>` / `bd update <id> --claim`
4. **Commit locally** — Every logical change should land as its own
   commit on `main`. NEVER stop with uncommitted work in the working
   tree (that's the failure mode wiaconstructor-5kcj documented).
5. **Verify** — `git status` shows clean working tree; `bd list
   --status=in_progress` is empty or accurately reflects active work
6. **Hand off** — Provide a brief context summary for the next session

**IF A REMOTE IS LATER ADDED:** restore the push step
(`git pull --rebase && git push`) between steps 4 and 5. Until then,
treat local commits as authoritative.
<!-- END BEADS INTEGRATION (block edited locally — wiaconstructor-uqvd; bd init may regenerate, re-apply the no-remote workflow if so) -->


## Build & Test

_Add your build and test commands here_

```bash
# Example:
# npm install
# npm test
```

## Architecture Overview

_Add a brief overview of your project architecture_

## Conventions & Patterns

_Add your project-specific conventions here_
