---
name: ship-issue
description: Use when taking a GitHub issue (or one coherent chunk of a roll-up) from pick through merge — chunk selection, worktree, TDD, PR, red-CI triage, issue bookkeeping, cleanup.
disable-model-invocation: true
---

# Ship an issue

Take one issue, or one coherent chunk of a large one, all the way to merged. Small and finished beats large and half-done.

## 0. Read the arguments

Every argument is optional. `/ship-issue` with no arguments is a valid, complete invocation.

- **Issue number** — optional. If absent, run `gh issue list` and pick one yourself: prefer the issue with a chunk that is cohesive, currently blocking, and landable in a single PR. State which you picked and why before touching anything.
- **Chunk hint** (e.g. `145 picker`) — optional, scopes to one section of a roll-up.
- **Merge authorization** — optional. See step 9. Only an explicit instruction counts (`merge when green`, `merge when done`, `don't ask`). Absent that, step 9 stops and asks.

Examples: `/ship-issue` · `/ship-issue 145` · `/ship-issue 145 picker` · `/ship-issue 145 picker, merge when green`

## 1. Pick the chunk

`gh issue view <n>`. For a roll-up issue, choose a group that is cohesive, independently testable, and small enough to land in one PR.

**Verify every item is still real before writing code.** Roll-ups go stale — read the current source for each one. Items already fixed by a later PR are common. Drop them and say so.

**Reproduce the bug and read the actual failure before you believe any explanation of it** — including one written on the issue, including one you wrote yourself. A measured failure *rate* is not a diagnosis. An issue's stated mechanism is a hypothesis until a captured failure agrees with it, and a plausible mechanism that explains the symptom is not thereby the true one.

If the bug will not reproduce, make it reproduce before theorising: vary load (`for i in $(seq 1 10); do yes > /dev/null & done`), concurrency, or ordering. A bug that hides on an idle machine is a timing bug, and guessing at timing bugs is how you ship a fix that changes nothing.

State the chunk and what you are leaving out, before starting.

### If the investigation invalidates the issue

It happens, and it is not a detour — it is the finding. When the real mechanism turns out to differ from what the issue claims:

1. **Correct the issue body first**, before writing any fix. Record the confirmed mechanism, the evidence for it, and **every hypothesis you refuted**, so the next reader does not re-walk them.
2. **Re-scope.** One issue may turn out to be two problems with different confidence levels, or a smaller problem than filed.
3. Only then continue. A fix built on a wrong premise passes CI and fixes nothing.

## 2. Isolate

`EnterWorktree`, then rename the branch to the house convention:

```
git branch -m fix/<slug>-<issue-number>     # or feature/*, chore/*
```

Never work directly in the main checkout — parallel sessions race on shared HEAD.

## 3. TDD — RED first

Per `superpowers:test-driven-development`. Write the failing test, **run it, and confirm it fails for the intended reason.** That ordering is the whole value: a test written after the fix proves nothing about whether it can catch the regression.

Record the RED result. It goes in the PR body.

If a change genuinely cannot be tested (CSS in jsdom, a platform-specific path), **say so explicitly** in the commit and PR. Do not invent a test that only restates the implementation.

### When the test *is* the artifact

Repairing a flaky or wrong test inverts this step: there is nothing to write a failing test *for*. RED is a **failure rate under a stated reproduction protocol**, not a single run.

1. Pin the protocol — the exact command and the load under which it fails.
2. Measure before. `n` runs, record the count.
3. Fix.
4. Measure after, **same protocol, same load**.

A single green run proves nothing about a bug that only appeared 1 run in 5.

### Two hazards while measuring

- **Never edit the tree while a measurement loop is running in it.** The loop tests whatever is on disk at the moment each run starts, so an edit halfway through silently mixes old and new results. Wait for it, or measure in a throwaway worktree.
- **Background load generators escape shell job control.** `yes > /dev/null &` inside a non-interactive shell is not tracked by `jobs -p`, so a trailing `kill $(jobs -p)` silently misses them and they burn CPU indefinitely. Arm an independent watchdog (`nohup sh -c 'sleep 600; pkill -x yes' &`) and verify with `pgrep -x yes` afterwards.

## 4. Verify before claiming anything

```
npx vitest run          # frontend
cargo test --workspace  # if Rust changed
npm run build           # tsc + vite
```

Quote the actual numbers. "Tests pass" while anything was skipped is a false report.

## 5. Commit

Conventional commits, signed. Explain **why** each defect mattered — the user-visible failure, not the diff.

Backticks in `-m` get eaten by zsh and a hook scans heredocs; if a message is mangled or refused, write it to a file and use `git commit -F <file>`.

If 1Password errors on signing, unlock and retry once. Do not pre-check `op whoami` — it false-negatives.

## 6. Push

**Claude cannot `git push`.** Ask the user to run:

```
! git push -u origin <branch>
```

## 7. Open the PR

`gh pr create --base main`. Body covers: what each defect was in user terms, the RED→GREEN evidence, anything left untested and why, and what was deliberately left out of scope.

Reference the issue — `Refs #N`. See step 10 before using `Closes`.

## 8. CI

Required checks: `frontend`, `rust (ubuntu-22.04)`.

Arm a `Monitor` on `gh pr checks <n>` rather than polling.

### If CI goes red — do not blind re-run

A re-run that happens to pass teaches nothing and retires the test. Triage:

1. **Read the actual failure.** Which test, which assertion.
2. **Reproduce locally.** Single file first, then the full suite — some failures only appear under parallelism.
3. **Baseline against `main`** before blaming or absolving your diff:
   ```
   git worktree add --detach .claude/worktrees/baseline-main <parent-sha>
   ```
   Loop the suite there. `node_modules` resolves from the repo root.
4. **Then decide.** Pre-existing flake → re-run, and **file it as its own issue** with the measured failure rate. Your bug → fix it.

Either way, comment on the PR with what you found. A red that got re-run without explanation is indistinguishable from a red that was ignored.

## 9. Merge gate

**Stop and ask before merging**, unless the user authorized it — as an argument, or at any point during this run. Merging to protected `main` is hard to reverse; approval for one issue is not a standing grant for the next.

With authorization, and only once every required check is green:

```
gh pr merge <n> --admin --squash
```

Then confirm it actually landed — a 502 does not mean it failed:

```
gh pr view <n> --json state,mergedAt,mergeCommit
```

## 10. Issue bookkeeping

Match the close to the issue's shape:

- **Single-purpose issue, fully fixed** → `Closes #N` in the PR body.
- **Roll-up issue** (#145-style) → **never auto-close.** `Refs #N`, then comment a checklist: `[x]` per item fixed, `[ ]` per item left with a one-line reason. Name the sections untouched.

Anything discovered along the way that is not this chunk gets **its own issue**, not a quiet expansion of this PR.

### Do not let mixed confidence look uniform

A PR often carries changes verified to different standards — a fix reproduced and re-measured, alongside sites that merely share the shape and were hardened on inspection. **Label which is which**, in the commit and the PR body. Unmeasured hardening is worth shipping; it is not worth passing off as verified, and a reviewer cannot tell the difference from the diff alone.

Same rule when one issue turns out to hold two problems: close only what the evidence covers, and say what the rest is.

## 11. Clean up

```
gh api -X DELETE repos/rhurling/convertbar/git/refs/heads/<branch>
```

`gh pr merge --delete-branch` errors when the branch is checked out in a worktree — the merge still lands, but the cleanup does not.

Then `ExitWorktree` (remove), and:

```
git checkout main && git pull --ff-only
git branch -D <branch>      # yes, -D — see below
```

**`ExitWorktree` does not delete the branch you renamed.** It removes the branch *it* created (`worktree-<name>`), and step 2 renamed that to `fix/<slug>-<n>` — so the rename orphans it and the branch survives the worktree removal. Delete it explicitly.

It must be `-D`, not `-d`: `main` takes squash merges, so your commits never become ancestors of `main` and `-d` refuses with "not fully merged" no matter how thoroughly the PR landed. **Confirm the merge before force-deleting** — the safety `-d` would have given you is exactly what `-D` discards:

```
gh pr list --state all --head <branch> --json number,state,mergedAt
```

`git branch -D` may be blocked by the auto-mode classifier. If it is, hand the user the command to run with `!` rather than reaching for `git update-ref -d` — that bypasses the intent of the block.

Verify: `git worktree list`, `git branch -vv` (no leftover `[origin/…: gone]` rows for this work), `git status`, `git log --oneline -2`.

## 12. Report

What merged, what you left open and why, anything you could not verify. If a step was skipped, say which. Completion claims need evidence attached.
