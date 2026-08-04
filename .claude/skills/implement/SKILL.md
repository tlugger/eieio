---
name: implement
description: Pick up a ready beads issue and drive it to a pushed, closed, spec-conformant implementation through a gated plan → fan-out → verify → review workflow. Use when the user asks to implement, build, or pick up a beads issue (optionally passing an issue id, e.g. /implement eieio-s85.3).
---

# Implement a beads issue

You are the **driving agent** for one beads issue, end to end: research → human scoping → written plan → human review gate → implementation (directly, or via parallel Sonnet subagents in worktrees) → integration & verification → code review → human commit gate → close.

**Model check (step 0):** This skill assumes an Opus-class or better driving session (Opus, Fable, Mythos). If you are running on a smaller model, say so and ask the user whether to continue anyway — do not silently proceed.

Non-negotiables from CLAUDE.md that this workflow must never erode:

- **Spec-first.** Never implement past a spec. Spec amendments and PROPOSED ratifications are planned changes, made only by you (the driver), and land in the same commit as the code they govern.
- **Subagents never touch `docs/`.** If a subagent hits a spec gap, it stops and reports; you amend the spec.
- ★ crates (`signal`, `expr`, `manifest`, `host-core`, `block-sdk`) stay `no_std` (+`alloc`).
- Commits: emoji-prefixed, grouped by area, no trailers, direct to `main`.

## Step 0 — Resolve and claim the issue

- If the user passed an issue id, use it. Otherwise run `bd ready`; if exactly one issue is ready, propose it; if several, ask the user which (AskUserQuestion).
- `bd show <id>` — read the description, acceptance criteria, and the parent epic (`bd show <epic>`) for shared context. The acceptance criteria are the definition of done; treat them as a checklist you will walk in step 6.
- `bd update <id> --claim`.

## Step 1 — Research

Build a complete picture before proposing anything:

- Read the governing spec sections cited by the issue (and `docs/SCOPE.md` where referenced). Note every **PROPOSED** marker the work would ratify and every point where the spec is silent or ambiguous for what you're building.
- Explore the existing code: what exists, what this change touches, blast radius (callers, shared crates, conformance suites, justfile recipes, CI).
- Check `bd list` for adjacent open issues so you don't implement work planned elsewhere.

## Step 2 — Human scoping (only genuine questions)

Ask the user (AskUserQuestion) **only** about things their decision can genuinely shape: spec ambiguities, real trade-offs, scope boundaries, naming that will become public contract. Prioritize spec gaps — those are decisions, not implementation details. If there is nothing worth asking, say so explicitly and move on. Never invent questions to appear thorough.

## Step 3 — Plan files

Write the plan as files in the repo (they are review artifacts for the user and input for subagents). Naming convention — **`*.eio-plan.md` is gitignored**; verify the pattern is in `.gitignore` and add it if missing:

- **Top-level plan** — `<issue-id>.eio-plan.md` at repo root:
  - The overall change: files to create / modify / delete (including spec edits and marker removals).
  - Decisions made (from step 2 and your own), each with a one-line rationale.
  - **Fan-out decision:** N subagents and why. **N=1 means no subagents at all — you implement directly in this session.** Only fan out when the work splits into genuinely distinct, isolated areas with disjoint file ownership (e.g. independent golden blocks). Anything two workstreams would both touch stays with you.
  - Verification steps for step 6 (commands to run, acceptance criteria mapped to how each will be checked).
  - Locations of sub-plans (if N>1).
- **Sub-plans** (only if N>1) — `<issue-id>.<area>.eio-plan.md` placed in the directory they concern. Each must be **fully self-contained** for a Sonnet subagent that has none of your context:
  - The issue excerpt and acceptance criteria it serves.
  - Governing spec content **quoted verbatim** — including any spec amendments you've drafted but not committed (worktrees branch from HEAD, so uncommitted spec edits are invisible to subagents; the sub-plan is how they see them).
  - Relevant CLAUDE.md invariants (no_std, copies-not-references, emit-enqueues, etc. as applicable).
  - The exact file list this subagent owns (disjoint from every other sub-plan).
  - Code examples, patterns to follow, and concrete test cases to implement.
  - Definition of done: tests to pass, commands to run.

Record the plan's key decisions on the bead as you go: `bd update <id> --design "..."` — the plan files get deleted later; the bead is the durable record.

## Step 4 — Review gate (loop until approved)

Prompt the user to review the plan files (AskUserQuestion): option **Approve**, plus free-text feedback via "Other". On feedback: update the plan files (and `--design`), then return to this gate. Do not proceed on anything but an explicit Approve.

## Step 5 — Implement

**If N=1:** implement directly in this session. No worktree, no subagent ceremony.

**If N>1:** for each sub-plan:

1. `git worktree add <repo-parent>/eieio-wt-<area> -b wt/<issue-id>/<area>` (branch from current HEAD).
2. Spawn all subagents **in parallel in one message** (Agent tool, `model: "sonnet"`). Each prompt must include: the absolute worktree path (work only there), the absolute path to its sub-plan in the main checkout (read it first), and these standing orders: implement only the files the sub-plan owns; never edit `docs/` or any `*.eio-plan.md`; run the plan's tests until green; commit checkpoints on your worktree branch; if the spec or plan is ambiguous or wrong, STOP and report back rather than improvising.
3. If a subagent reports a spec gap: resolve it (amend spec / plan, consult the user if it's a step-2-grade decision), update the sub-plan, and send the subagent back to work (SendMessage to the same agent keeps its context).

## Step 6 — Integrate and verify

- If N>1: in the main checkout, `git merge --squash wt/<issue-id>/<area>` for each branch (subagent checkpoint commits never reach main's history), resolve conflicts yourself, then `git worktree remove` each tree and delete the `wt/` branches.
- Run the full gates: `just ci` (or, before the justfile exists, the equivalent fmt/clippy/build/test commands).
- Walk the issue's **acceptance criteria as a literal checklist**. Verify the change conforms to the governing specs — read the diff, don't trust the reports. Make any adjustments yourself, now.
- Confirm spec/code pairing: every spec amendment or PROPOSED-marker removal is present and will land with its code.
- File follow-up beads for anything discovered but out of scope.
- Record verification evidence in the bead: `bd update <id> --notes "..."`.
- Delete all `*.eio-plan.md` files.

## Step 7 — Code review

Run the `/code-review` skill on the working diff. Triage its findings:

- Fix what matters for this feature.
- Dismiss what is out of scope, already planned in another bead, or wrong — but **record every dismissal** with its reason in the bead's notes (or as a filed follow-up bead). Nothing is silently dropped.

## Step 8 — Commit gate (loop until committed)

Propose the commit breakdown per CLAUDE.md: per-area commits, best-fit emoji, spec+code together, no trailers. Then prompt (AskUserQuestion): option **Commit and push**, plus free-text feedback via "Other". On feedback: make the fixes, re-run affected gates, and return to this gate. On approval:

1. Create the commits as proposed; `git push`.
2. `bd close <id>` (with `--reason` if useful); ensure follow-up beads from steps 6–7 are filed.
3. `bd dolt push`.

The skill ends here. Report: what landed (commits), issue closed, follow-ups filed, and anything the next issue's driver should know.
