# Agent workflow

The plan is split so each session fits comfortably in context and produces a verifiable increment.

## 1. Session model

One unchecked box in `plan.md` = one session. A session should take roughly 1–3 hours. If it will take longer, split the box before starting.

Never start a box whose dependencies are unchecked. The graph is in `plan.md`.

## 2. Prompt template

Paste this into a fresh session:

```text
Read only:
- plan.md, task <ID>
- <linked doc>, section <N>

Goal: implement <ID> exactly as scoped.
Constraints:
- Do not touch files outside the "Touch" list unless strictly required.
- Keep the change minimal; prefer new modules over refactors.
- Add tests and run the task's "Done" command.
- If scope grows, stop and propose sub-tasks instead of implementing them.

When finished:
- Check the box for <ID> in plan.md.
- Append a session entry to docs/session-log.md.
- Report: files changed, tests run, gate evidence, open issues.
```

## 3. Context hygiene

- Do not read the entire repository. Use `rg` to find a symbol, then read the enclosing file.
- Do not read all of `docs/`; the task names the section.
- Keep diffs small. If a helper is needed by two tasks, add it in the earlier task and document it.
- Do not paste large files into the prompt; reference paths.
- One logical change per session. Do not mix a refactor with a feature.

## 4. Definition of done

A session is complete only when all of these hold:

- `cargo fmt --check` passes.
- `cargo clippy -- -D warnings` passes.
- `cargo test` passes, including the new tests.
- The task's specific "Done" command passes.
- The linked doc is updated if public behavior changed.
- The checkbox is checked and the session log entry is written.

If a gate cannot be met, leave the box unchecked and record the blocker in the log. Do not mark partial work complete.

## 5. Session log

Append to `docs/session-log.md`:

```markdown
## S1.2 — 2026-11-07

- Scope: phase-correlation translation
- Changed: src/align/phase.rs, tests/align_phase.rs
- Evidence: `cargo test align_phase` (14 passed); synthetic residual 0.03 px
- Decisions: parabolic subpixel fit; upsampled DFT deferred to S1.4 if needed
- Follow-ups: none
```

Keep entries short. The log is for the next session, not for a human reader.

## 6. Escalation

Stop and ask when:

- A gate requires a design decision not covered by `docs/decisions.md`.
- A crate cannot do something the plan assumes (e.g., ICC tag writing).
- A change would touch more than the listed files.
- The task cannot be verified with an automated test.

Record the question and the options considered; do not guess silently.

## 7. Automated loop

`scripts/ralph.py` implements this workflow mechanically: one fresh OpenCode
session per unchecked task, standard gates, checkbox update, session-log
entry, and a commit. The prompt template is built by the script from the task
block, so the manual template above is only needed when running a session by
hand.

See `docs/ralph.md` for usage, safety, and resume behavior.

