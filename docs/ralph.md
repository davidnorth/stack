# Ralph loop

`scripts/ralph.py` runs the build plan unattended: one OpenCode session per
unchecked task in `plan.md`, verified by the standard gates, committed, and
logged.

It is a *structured* Ralph loop: the agent does not choose the next task. The
script does, from the plan, so dependencies and context stay controlled.

## 1. What one iteration does

1. Read `plan.md` and pick the next unchecked task (`S0.1`, `S1.2`, ...).
2. Build a small prompt containing only that task block and the loop rules.
3. Start a **fresh** `opencode run` session (agent `build`, tools auto-approved).
4. Run the gates:
   - `cargo fmt --check`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo test`
5. Verify the task checkbox is checked. If gates pass but the agent forgot,
   the loop checks it (unless `--require-checkbox`).
6. Commit the stage: `ralph(S1.2): Phase-correlation translation`.
7. Append a session-log entry if the agent did not.
8. Move to the next task.

On failure, the same task is retried in a new session with the gate output and
log tail appended to the prompt. After the last attempt the loop stops (unless
`--continue-on-failure`), leaving the box unchecked.

## 2. Prerequisites

- `opencode` v2 on `PATH`, authenticated, with a usable default model.
- Rust toolchain with `rustfmt`, `clippy`, and `cargo` on `PATH`.
- A git identity (`git config user.email`) if you want per-stage commits.
- The repo does not need to be a git repo for the loop to run; `S0.1` creates
  it. Without one, commits are skipped.

Check the setup:

```bash
opencode models
rustup default stable
rustup component add rustfmt clippy
cargo --version
git config user.email
```

## 3. Usage

```bash
# Show all tasks and their status.
./scripts/ralph.py --list

# Inspect the first task's prompt and exact command without spending tokens.
./scripts/ralph.py --dry-run --only S0.1

# Run one stage, then stop.
./scripts/ralph.py --max-stages 1

# Run the whole plan with an explicit model.
./scripts/ralph.py --model deepseek/deepseek-flash

# Run a range.
./scripts/ralph.py --from S1.1 --until S3.2

# Retry a stage that is already marked done.
./scripts/ralph.py --only S5.2 --rerun
```

Recommended first run: `--max-stages 1`, inspect the diff and commit, then
continue with the same command. Rerunning resumes at the first unchecked task.

If the default model's key is expired, select a working provider explicitly:

```bash
./scripts/ralph.py --model deepseek/deepseek-flash --max-stages 1
# or:
RALPH_MODEL=deepseek/deepseek-flash ./scripts/ralph.py
```

## 4. Options

| Option | Default | Notes |
|---|---|---|
| `--only S0.1[,S0.2]` | all | repeatable, comma-separated |
| `--from S0.1` | first | inclusive |
| `--until S1.6` | last | inclusive |
| `--max-stages N` | 0 (all) | stop after N tasks |
| `--list` | | print tasks and exit |
| `--dry-run` | | print commands and the first prompt |
| `--attempts N` | 2 | each attempt is a fresh session |
| `--retry-delay S` | 10 | seconds between attempts |
| `--timeout S` | 2700 | per-attempt wall clock |
| `--gate-timeout S` | 1800 | per-gate wall clock |
| `--sleep S` | 2 | seconds between tasks |
| `--model provider/model[#variant]` | OpenCode default | or `RALPH_MODEL` |
| `--agent NAME` | `build` | or `RALPH_AGENT` |
| `--standalone` | off | private OpenCode server |
| `--debug` | off | `--print-logs --log-level debug` |
| `--no-commit` | off | leave changes uncommitted |
| `--continue-on-failure` | off | proceed to later tasks |
| `--require-checkbox` | off | disable auto-check; fail if unchecked |
| `--skip-gates` | off | checkbox-only verification (debug) |
| `--rerun` | off | include already-done tasks |
| `--extra "..."` | | appended to every prompt |

## 5. State, logs, resume

```text
.ralph/
  state.json              per-task status, attempts, commit, log, prompt
  runs/<run-id>/
    S0.1-attempt1.log     full OpenCode output + gate output
    S0.1-attempt1.prompt.md  exact prompt sent to that session
    summary.md            table of the run
```

- `plan.md` checkboxes are the human-readable tracker.
- `.ralph/state.json` is a cache that survives a forgotten checkbox and speeds
  up resume.
- To reset a stage: uncheck its box and run `--only Sx.y --rerun`.
- To reset everything: `rm -rf .ralph` and uncheck the boxes you want redone.
- `.ralph/` and `target/` are gitignored.

## 6. Safety

- `opencode run --auto` auto-approves tools that are not explicitly denied.
  The loop is intended to edit this repository, not to be a sandbox.
- Run it on a dedicated branch or in a git worktree so you can review or reset:
  ```bash
  git switch -c ralph/build
  ./scripts/ralph.py
  ```
- The loop commits after each verified stage, which makes `git log` the
  progress record and `git revert` the undo button.
- It stops on the first failure by default. Read the log before rerunning.
- `--skip-gates` and `--require-checkbox` are for debugging the loop, not for
  normal builds.

## 7. Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `opencode not found` | Add the OpenCode bin directory to `PATH`. |
| `cargo not found` | Install Rust; S0.1 needs it. |
| `This API key has expired` | The default model's provider key is expired. Pass `--model <provider/model>` or set `RALPH_MODEL`. In this workspace `deepseek/deepseek-flash` is configured and working. |
| Stage fails on `Cargo.toml not found` | S0.1 did not create the crate; inspect its log. |
| Commits are skipped | Set `git config user.email` and `user.name`. |
| Agent does nothing | Check `opencode models` and pass `--model`; verify the default agent is `build`. |
| Loop wedges on one task | Read `.ralph/runs/<id>/*-attempt*.log`; rerun with `--only Sx.y --attempts 1 --debug`. |
| Stage marked done but wrong | `git revert <commit>`, uncheck the box, run `--only Sx.y --rerun`. |
| Too much context per stage | Split the checkbox into sub-tasks in `plan.md`; the loop picks them up automatically. |
