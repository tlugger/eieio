---
name: implement
description: Continuously pick up ready beads issues and drive each one to a pushed, closed, spec-conformant implementation — research → scoping questions → plan → implement (directly or via parallel Sonnet subagents) → verify → review → commit → close → next issue. Stops only for genuine scoping questions, a hard failure, or an empty queue. Use when the user asks to implement, build, or pick up beads issues (optionally passing an issue id, e.g. /implement eieio-s85.3).
---

# Implement beads issues, continuously

You are the **driving agent**. You run this cycle per issue and then **start over on the next ready issue**, without being asked:

```
select → research → ASK (the one gate) → plan → implement → verify → review → commit+push → close → ↻
```

**There is exactly one gate: step 2.** Everything after it runs unattended. Do not ask for plan approval, do not ask for commit approval, do not ask whether to continue to the next issue. The user opted into all of that by invoking this skill.

**Authorization.** Invoking this skill grants standing authority to commit and push to `main` for the issues you work, overriding the conservative default in the CLAUDE.md Beads block. It does **not** authorize force-pushing, history rewriting, deleting or renaming remote branches, touching CI secrets, or committing files unrelated to the current issue. Those still stop and ask.

**Model check:** this assumes an Opus-class or better driving session. On a smaller model, say so and ask whether to continue before starting.

Non-negotiables from CLAUDE.md that this workflow must never erode:

- **Spec-first.** Never implement past a spec. Spec amendments and PROPOSED ratifications are yours alone (never a subagent's) and land in the same commit as the code they govern.
- **Subagents never touch `docs/`.** A subagent that hits a spec gap stops and reports; you amend the spec.
- ★ crates (`signal`, `expr`, `manifest`, `host-core`, `block-sdk`) stay `no_std` (+`alloc`).
- Commits: emoji-prefixed, grouped by area, no trailers, direct to `main`.

## Step 0 — Select and claim (no gate)

- If the user named an issue, use it. On later loop iterations, choose yourself — do not ask.
- `bd ready`. Skip `[epic]` rows: epics are containers and close when their children do (unless an epic has no children). Among the rest pick, in order: lowest priority number (P0 first) → whatever unblocks the most downstream work (`bd show` BLOCKS) → the SCOPE §7.1 epic sequence.
- Announce the pick in one line with the reason. If nothing is ready, **stop and report** — that is the end of the loop.
- `bd show <id>` plus the parent epic for shared context. The acceptance criteria are the definition of done; you will walk them literally in step 5.
- `bd update <id> --claim`.

## Step 1 — Research

- Read the governing spec sections the issue cites (and `docs/SCOPE.md` where referenced). Note every **PROPOSED** marker this work would ratify, and every point where a spec is silent, ambiguous, or wrong for what you are about to build.
- Explore the code: what exists, what this touches, blast radius (callers, shared crates, conformance suites, justfile recipes, CI).
- `bd list` for adjacent open issues, so you don't implement work planned elsewhere.
- Probe mechanisms empirically now, before they reach the plan. A one-command experiment that kills a bad approach is worth more than a paragraph of reasoning about it.

## Step 2 — Scoping questions (THE GATE)

Ask (AskUserQuestion) **only** what the user's decision genuinely shapes: spec gaps and ambiguities, real trade-offs, scope boundaries, naming that becomes public contract. Spec gaps come first — those are decisions, not implementation details.

Ground every option in what you actually found in step 1: name the file, the taken crate name, the failing command. Options the user can't distinguish are worthless.

**If there is nothing genuine to ask, say so in one line and proceed.** Never manufacture a question to create a checkpoint — a trivial issue should run start to finish untouched.

This gate reopens mid-flight, and only for this: a decision of step-2 grade surfaces during implementation (a spec is silent on something load-bearing, or the work turns out to contradict a settled decision). Then stop and ask. Anything smaller, decide yourself and record it.

## Step 3 — Plan (no approval)

Write the plan to `<issue-id>.eio-plan.md` at the repo root (`*.eio-plan.md` is gitignored — verify the pattern is present, add it if not). It is your working artifact and the input for subagents, not a document awaiting sign-off. Cover:

- Files to create / modify / delete, including spec edits and marker removals.
- Decisions, each with a one-line rationale — from step 2 and your own.
- **Fan-out:** N and why. **N=1 means no subagents — you implement directly.** Fan out only for genuinely distinct areas with disjoint file ownership (e.g. independent golden blocks). Anything two workstreams would both touch stays with you.
- Verification: the commands to run, each acceptance criterion mapped to how it gets checked, and how you will prove each new gate can **fail**.

**Sub-plans** (only if N>1) — `<issue-id>.<area>.eio-plan.md` in the directory it concerns, fully self-contained for a Sonnet subagent with none of your context: the issue excerpt and its acceptance criteria; governing spec content **quoted verbatim**, including spec amendments you have drafted but not committed (worktrees branch from HEAD, so uncommitted edits are invisible — the sub-plan is how they see them); applicable CLAUDE.md invariants; the exact disjoint file list it owns; patterns to follow and concrete test cases; definition of done.

Record the durable version on the bead as you go — `bd update <id> --design "..."` — because the plan file gets deleted in step 5.

Then go straight to step 4.

## Step 4 — Implement

**N=1:** implement directly. No worktree, no subagent ceremony.

**N>1:** per sub-plan, `git worktree add <repo-parent>/eieio-wt-<area> -b wt/<issue-id>/<area>` from HEAD, then spawn all subagents **in parallel in one message** (Agent tool, `model: "sonnet"`). Each prompt carries: the absolute worktree path (work only there), the absolute path to its sub-plan in the main checkout (read it first), and the standing orders — implement only the files you own; never edit `docs/` or any `*.eio-plan.md`; run the plan's tests until green; commit checkpoints on your worktree branch; if the spec or plan is ambiguous or wrong, STOP and report rather than improvising. On a reported spec gap: resolve it yourself (amend spec and sub-plan; ask only if it is step-2 grade), then SendMessage the same agent back to work so it keeps its context.

## Step 5 — Integrate and verify

- If N>1: `git merge --squash wt/<issue-id>/<area>` per branch in the main checkout (subagent checkpoints never reach main's history), resolve conflicts yourself, then `git worktree remove` each tree and delete the `wt/` branches.
- Run `just ci` (before the justfile exists, the equivalent fmt/clippy/build/test commands). It must be green.
- Walk the acceptance criteria as a literal checklist. **Read the diff — never trust a subagent's report.** Verify conformance against the governing specs and fix what's off, now.
- **Prove new gates can fail.** A gate verified only in the passing direction is unverified: break the input deliberately, confirm the failure and message, revert. Same for any claim you intend to write down — test it instead of asserting it.
- Confirm spec/code pairing: every spec amendment and PROPOSED-marker removal is present and lands with its code.
- File follow-up beads for anything found but out of scope.
- `bd update <id> --notes "..."` with the evidence: commands run, exit codes, negative tests, and every review dismissal with its reason.
- Delete all `*.eio-plan.md` files.

## Step 6 — Review the diff

Run the `/code-review` skill on the working diff if it is installed; if it isn't, review the diff yourself with the same rigor and say which you did. Fix what matters for this feature. Dismiss what is out of scope, planned elsewhere, or wrong — but **record every dismissal** in the bead notes or as a filed bead. Nothing is silently dropped.

## Step 7 — Commit and push (no gate)

Commit per CLAUDE.md: one commit per area, best-fit emoji, spec+code together, no trailers, `main` directly. Then `git push`.

- Commit only what belongs to this issue, plus beads bookkeeping (`.beads/*.jsonl`, 📋) so the tree ends clean for the next iteration.
- **Unrelated dirty files that you did not create: stop and ask.** Never sweep unknown changes into a commit.
- If `just ci` is red and you cannot fix it, **stop the loop**: do not commit, leave the bead claimed, record what failed in the notes, and report. Same for a push that is rejected — report the exact error rather than reaching for `--force`.

## Step 8 — Close, then loop

1. `bd close <id>` with `--reason`. If that was an epic's last child and the epic's own criteria are met, close the epic too.
2. Confirm the follow-up beads from steps 5–6 exist.
3. `bd dolt push`.
4. Report this iteration compactly: commits landed, issue closed, follow-ups filed, and what the next driver needs to know.
5. **Return to step 0 immediately.**

## Stop conditions

Stop the loop and report — do not continue to the next issue — when: `bd ready` is empty; `just ci` cannot be made green; a push fails; the same issue has failed twice; the tree holds unrelated changes you did not make; or an action would need authority this skill does not grant (force-push, history rewrite, remote branch deletion, secrets).

Otherwise keep going. The user interrupts if they want you to stop.
