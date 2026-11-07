#!/usr/bin/env python3
"""Ralph loop: one OpenCode session per unchecked task in plan.md.

Each stage gets a fresh session. Failed verification retries the same stage in
a new session, with the failure output appended to the prompt. Progress is
tracked in plan.md (checkbox) and .ralph/state.json (cache).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import shutil
import signal
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PLAN = ROOT / "plan.md"
RALPH_DIR = ROOT / ".ralph"
STATE_FILE = RALPH_DIR / "state.json"
RUNS_DIR = RALPH_DIR / "runs"
TASK_RE = re.compile(r"^- \[([ xX])\] \*\*(S\d+\.\d+) (.+?)\*\*\s*$")

CURRENT_PROC: subprocess.Popen[str] | None = None


@dataclass
class Task:
    line_no: int
    checked: bool
    id: str
    title: str
    block: str


def die(message: str, code: int = 2) -> None:
    print(f"ralph: {message}", file=sys.stderr)
    raise SystemExit(code)


# ---------------------------------------------------------------------------
# Plan parsing
# ---------------------------------------------------------------------------


def parse_tasks() -> list[Task]:
    lines = PLAN.read_text(encoding="utf-8").splitlines()
    tasks: list[Task] = []
    start: int | None = None

    for i, line in enumerate(lines):
        match = TASK_RE.match(line)
        if match:
            if start is not None:
                tasks.append(make_task(lines, start, i))
            start = i
        elif start is not None and (line.startswith("---") or line.startswith("## ")):
            tasks.append(make_task(lines, start, i))
            start = None

    if start is not None:
        tasks.append(make_task(lines, start, len(lines)))

    return tasks


def make_task(lines: list[str], start: int, end: int) -> Task:
    match = TASK_RE.match(lines[start])
    assert match is not None
    return Task(
        line_no=start,
        checked=match.group(1).lower() == "x",
        id=match.group(2),
        title=match.group(3).strip(),
        block="\n".join(lines[start:end]).rstrip(),
    )


def set_task_checked(task_id: str, checked: bool) -> bool:
    lines = PLAN.read_text(encoding="utf-8").splitlines(keepends=True)
    for i, line in enumerate(lines):
        match = TASK_RE.match(line.rstrip("\n"))
        if match and match.group(2) == task_id:
            mark = "x" if checked else " "
            lines[i] = re.sub(
                r"^(- )\[[ xX]\]", rf"\1[{mark}]", line, count=1
            )
            PLAN.write_text("".join(lines), encoding="utf-8")
            return True
    return False


def is_checked(task_id: str) -> bool:
    return any(t.id == task_id and t.checked for t in parse_tasks())


# ---------------------------------------------------------------------------
# State
# ---------------------------------------------------------------------------


def load_state() -> dict:
    if not STATE_FILE.exists():
        return {"version": 1, "tasks": {}}
    try:
        state = json.loads(STATE_FILE.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        die(f"cannot parse {STATE_FILE}: {exc}", 1)
    state.setdefault("version", 1)
    state.setdefault("tasks", {})
    return state


def save_state(state: dict) -> None:
    RALPH_DIR.mkdir(parents=True, exist_ok=True)
    tmp = STATE_FILE.with_name(STATE_FILE.name + ".tmp")
    tmp.write_text(
        json.dumps(state, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    tmp.replace(STATE_FILE)


def task_done(task: Task, state: dict) -> bool:
    return task.checked or state["tasks"].get(task.id, {}).get("status") == "done"


def task_status(task: Task, state: dict) -> str:
    if task.checked:
        return "done"
    recorded = state["tasks"].get(task.id, {}).get("status")
    if recorded == "done":
        return "done (state)"
    if recorded == "failed":
        return "failed"
    return "todo"


# ---------------------------------------------------------------------------
# Selection
# ---------------------------------------------------------------------------


def select_tasks(tasks: list[Task], args: argparse.Namespace) -> list[Task]:
    selected = list(tasks)
    all_ids = {t.id for t in tasks}

    if args.only:
        wanted: list[str] = []
        for chunk in args.only:
            wanted.extend(x.strip() for x in chunk.split(",") if x.strip())
        unknown = sorted(set(wanted) - all_ids)
        if unknown:
            die(f"unknown task id(s): {', '.join(unknown)}")
        wanted_set = set(wanted)
        selected = [t for t in selected if t.id in wanted_set]

    if args.start_at:
        index = next(
            (i for i, t in enumerate(selected) if t.id == args.start_at), None
        )
        if index is None:
            die(f"--from task not in selection: {args.start_at}")
        selected = selected[index:]

    if args.stop_at:
        index = next(
            (i for i, t in enumerate(selected) if t.id == args.stop_at), None
        )
        if index is None:
            die(f"--until task not in selection: {args.stop_at}")
        selected = selected[: index + 1]

    return selected


# ---------------------------------------------------------------------------
# Prompt and command
# ---------------------------------------------------------------------------


def build_prompt(
    task: Task, attempt: int, failure: str, extra: str
) -> str:
    parts = [
        'You are one iteration of an automated build loop ("ralph").',
        "Execute exactly one task from plan.md, then stop.",
        "",
        f"Repository: {ROOT}",
        f"Task: {task.id} — {task.title}",
        "",
        "Task block:",
        "```markdown",
        task.block,
        "```",
        "",
        "Rules:",
        "- Read only the files and doc sections named in the task. Do not read the whole repository.",
        "- Read docs/agent-workflow.md and follow its definition of done.",
        "- Do not start, modify, or check any other task.",
        "- Do not edit plan.md except to check this task's box.",
        "- Do not commit; the loop commits after verification.",
        "- Keep changes minimal and consistent with the existing code.",
        "- Run the standard gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.",
        f"- Check the box for {task.id} in plan.md only when the task is complete and the gates pass.",
        "- Append a session entry to docs/session-log.md.",
        "- If blocked, leave the box unchecked and explain the blocker in your final message.",
    ]

    if extra.strip():
        parts += ["", "Additional instructions:", extra.strip()]

    if attempt > 1 and failure:
        parts += [
            "",
            "A previous attempt failed verification. Fix these issues and complete the task:",
            "```text",
            failure[-8000:],
            "```",
        ]

    return "\n".join(parts)


def opencode_command(
    args: argparse.Namespace, task: Task, prompt: str
) -> list[str]:
    cmd = ["opencode"]
    if args.debug:
        cmd += ["--print-logs", "--log-level", "debug"]

    cmd += [
        "run",
        "--auto",
        "--agent",
        args.agent,
        "--title",
        f"ralph {task.id} — {task.title}",
    ]

    if args.model:
        cmd += ["--model", args.model]
    if args.standalone:
        cmd += ["--standalone"]

    cmd.append(prompt)
    return cmd


# ---------------------------------------------------------------------------
# Process execution
# ---------------------------------------------------------------------------


def terminate(proc: subprocess.Popen[str]) -> None:
    try:
        os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        return
    try:
        proc.wait(timeout=15)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass


def run_with_tee(
    cmd: list[str], log_path: Path, timeout: int, prompt_name: str = ""
) -> int:
    global CURRENT_PROC
    log_path.parent.mkdir(parents=True, exist_ok=True)

    env = os.environ.copy()
    env.setdefault("NO_COLOR", "1")
    env.setdefault("CARGO_TERM_COLOR", "never")

    with log_path.open("w", encoding="utf-8", errors="replace") as log:
        marker = f" <prompt: {prompt_name}>" if prompt_name else " <prompt>"
        log.write(
            "$ " + " ".join(shlex.quote(c) for c in cmd[:-1]) + marker + "\n\n"
        )
        log.flush()

        proc = subprocess.Popen(
            cmd,
            cwd=ROOT,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            errors="replace",
            bufsize=1,
            start_new_session=True,
        )
        CURRENT_PROC = proc

        def reader() -> None:
            assert proc.stdout is not None
            for line in proc.stdout:
                log.write(line)
                log.flush()
                sys.stdout.write(line)
                sys.stdout.flush()

        thread = threading.Thread(target=reader, daemon=True)
        thread.start()

        try:
            rc = proc.wait(timeout=timeout)
        except KeyboardInterrupt:
            terminate(proc)
            raise
        except subprocess.TimeoutExpired:
            print(
                f"\nralph: timeout after {timeout}s; terminating",
                file=sys.stderr,
            )
            terminate(proc)
            rc = 124
        finally:
            thread.join(timeout=10)
            if proc.poll() is not None:
                CURRENT_PROC = None

    return rc


# ---------------------------------------------------------------------------
# Verification
# ---------------------------------------------------------------------------


def run_gates(args: argparse.Namespace) -> tuple[bool, str]:
    if args.skip_gates:
        return True, "gates skipped by --skip-gates"

    if not (ROOT / "Cargo.toml").exists():
        return (
            False,
            "Cargo.toml not found. S0.1 must create the crate before gates can run.",
        )

    commands = [
        ["cargo", "fmt", "--check"],
        ["cargo", "clippy", "--all-targets", "--", "-D", "warnings"],
        ["cargo", "test"],
    ]
    env = os.environ.copy()
    env["CARGO_TERM_COLOR"] = "never"
    chunks: list[str] = []

    for command in commands:
        label = " ".join(command)
        try:
            result = subprocess.run(
                command,
                cwd=ROOT,
                env=env,
                capture_output=True,
                text=True,
                timeout=args.gate_timeout,
            )
        except FileNotFoundError:
            return False, "cargo not found on PATH"
        except subprocess.TimeoutExpired:
            return False, f"$ {label}\ntimed out after {args.gate_timeout}s"

        chunks.append(
            f"$ {label}\n{result.stdout}{result.stderr}\nexit={result.returncode}"
        )
        if result.returncode != 0:
            return False, "\n".join(chunks)

    return True, "\n".join(chunks)


def read_tail(path: Path, limit: int) -> str:
    try:
        return path.read_text(encoding="utf-8", errors="replace")[-limit:]
    except OSError:
        return ""


def is_key_error(text: str) -> bool:
    lowered = text.lower()
    return "api key" in lowered and any(
        marker in lowered
        for marker in ("expired", "invalid", "unauthorized", "forbidden")
    )


def print_key_hint() -> None:
    print(
        "  hint: the selected model's API key is not usable.",
        file=sys.stderr,
    )
    print(
        "        Pass --model <provider/model> or set RALPH_MODEL.",
        file=sys.stderr,
    )
    print(
        "        Example: ./scripts/ralph.py --model deepseek/deepseek-flash",
        file=sys.stderr,
    )


# ---------------------------------------------------------------------------
# Git and session log
# ---------------------------------------------------------------------------


def git(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args], cwd=ROOT, capture_output=True, text=True
    )


def in_git_repo() -> bool:
    return git("rev-parse", "--is-inside-work-tree").stdout.strip() == "true"


def ensure_gitignore() -> None:
    path = ROOT / ".gitignore"
    text = path.read_text(encoding="utf-8") if path.exists() else ""
    existing = {
        line.strip().rstrip("/")
        for line in text.splitlines()
        if line.strip() and not line.strip().startswith("#")
    }
    canonical = [".ralph/", "target/", "__pycache__/", ".DS_Store"]
    needed = [entry for entry in canonical if entry.rstrip("/") not in existing]
    if needed:
        suffix = ("\n" if text and not text.endswith("\n") else "") + "\n".join(
            needed
        ) + "\n"
        path.write_text(text + suffix, encoding="utf-8")


def commit_task(task: Task, args: argparse.Namespace) -> str | None:
    if args.no_commit:
        return None
    if not in_git_repo():
        return None
    if not git("config", "user.email").stdout.strip():
        print(
            "ralph: git user.email is not set; skipping commit",
            file=sys.stderr,
        )
        return None

    added = git("add", "-A")
    if added.returncode != 0:
        print(f"ralph: git add failed: {added.stderr.strip()}", file=sys.stderr)
        return None

    if not git("status", "--porcelain").stdout.strip():
        return git("rev-parse", "--short", "HEAD").stdout.strip() or None

    committed = git("commit", "-m", f"ralph({task.id}): {task.title}")
    if committed.returncode != 0:
        print(
            f"ralph: git commit failed: {committed.stderr.strip()}",
            file=sys.stderr,
        )
        return None

    return git("rev-parse", "--short", "HEAD").stdout.strip()


def ensure_session_log(
    task: Task, log_rel: str, commit: str | None, evidence: str
) -> None:
    path = ROOT / "docs/session-log.md"
    text = path.read_text(encoding="utf-8") if path.exists() else "# Session log\n"

    if re.search(rf"(?m)^## {re.escape(task.id)}\b", text):
        return

    if commit:
        diff = git("show", "--stat", "--format=", commit).stdout.strip()
    else:
        diff = git("diff", "--stat").stdout.strip()

    entry = (
        f"\n## {task.id} — {datetime.now().strftime('%Y-%m-%d')} (ralph)\n\n"
        f"- Scope: {task.title}\n"
        f"- Changed: {diff or 'see working tree'}\n"
        f"- Evidence: {evidence}; log: `{log_rel}`\n"
        "- Decisions: see the agent session\n"
        "- Follow-ups: none recorded\n"
    )
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text.rstrip() + "\n" + entry, encoding="utf-8")


def write_summary(run_dir: Path, results: list[dict]) -> None:
    path = run_dir / "summary.md"
    lines = [
        f"# Ralph run {run_dir.name}",
        "",
        "| Task | Status | Attempts | Commit | Log |",
        "|---|---|---|---|---|",
    ]
    for result in results:
        lines.append(
            f"| {result['id']} | {result['status']} | {result['attempts']} "
            f"| {result.get('commit') or ''} | {result.get('log') or ''} |"
        )
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"\nralph: summary: {path.relative_to(ROOT)}")


# ---------------------------------------------------------------------------
# Main loop
# ---------------------------------------------------------------------------


def run(args: argparse.Namespace) -> int:
    if not PLAN.exists():
        die(f"missing {PLAN}", 2)

    tasks = parse_tasks()
    if not tasks:
        die("no tasks found in plan.md", 2)

    state = load_state()
    selected = select_tasks(tasks, args)

    if args.list:
        for task in selected:
            mark = "x" if task.checked else " "
            status = task_status(task, state)
            print(f"[{mark}] {task.id:<5} {task.title}  ({status})")
        return 0

    pending = [
        task for task in selected if args.rerun or not task_done(task, state)
    ]
    if args.max_stages > 0:
        pending = pending[: args.max_stages]

    if args.dry_run:
        print(f"ralph: dry run, {len(pending)} task(s) selected")
        for index, task in enumerate(pending):
            cmd = opencode_command(args, task, "")
            print(f"\n=== {task.id} — {task.title} ===")
            print(
                "$ "
                + " ".join(shlex.quote(part) for part in cmd[:-1])
                + " <prompt>"
            )
            if index == 0:
                print("\n--- prompt ---")
                print(build_prompt(task, 1, "", args.extra))
                print("--- end prompt ---")
            elif index == 2:
                print(
                    "\n... remaining prompts omitted; use --only or "
                    "--max-stages to inspect them"
                )
                break
        if not shutil.which("opencode"):
            print("\nralph: warning: opencode is not on PATH", file=sys.stderr)
        return 0

    if not pending:
        print("ralph: nothing to do")
        return 0

    if not shutil.which("opencode"):
        die("opencode not found on PATH", 2)
    if not args.skip_gates and not shutil.which("cargo"):
        die("cargo not found on PATH (use --skip-gates to bypass)", 2)

    ensure_gitignore()

    # Repair a stale checkbox from the state cache before running.
    if not args.rerun:
        repaired = False
        for task in selected:
            if (
                state["tasks"].get(task.id, {}).get("status") == "done"
                and not task.checked
            ):
                set_task_checked(task.id, True)
                repaired = True
        if repaired:
            print("ralph: repaired checked state from .ralph/state.json")

    run_id = datetime.now().strftime("%Y%m%d-%H%M%S") + f"-{os.getpid()}"
    run_dir = RUNS_DIR / run_id
    run_dir.mkdir(parents=True, exist_ok=True)

    print(f"ralph: {len(pending)} task(s) queued")
    print(f"  plan:  {PLAN}")
    print(f"  run:   {run_dir.relative_to(ROOT)}")
    print(f"  model: {args.model or '(opencode default)'}")
    print(f"  agent: {args.agent}")
    print("  note:  tools are auto-approved; press Ctrl-C to stop")

    results: list[dict] = []
    failures = 0

    for task in pending:
        print(f"\n=== {task.id} — {task.title} ===")
        success = False
        failure_text = ""
        attempt = 0
        commit: str | None = None
        log_rel = ""
        fatal_key_error = False

        for attempt in range(1, args.attempts + 1):
            print(f"  attempt {attempt}/{args.attempts}")

            prompt = build_prompt(task, attempt, failure_text, args.extra)
            cmd = opencode_command(args, task, prompt)
            log_path = run_dir / f"{task.id}-attempt{attempt}.log"
            prompt_path = log_path.with_name(f"{log_path.stem}.prompt.md")
            log_rel = str(log_path.relative_to(ROOT))
            prompt_rel = str(prompt_path.relative_to(ROOT))
            prompt_path.write_text(prompt, encoding="utf-8")

            rc = run_with_tee(cmd, log_path, args.timeout, prompt_path.name)
            agent_tail = read_tail(log_path, 8000)
            key_error = is_key_error(agent_tail)
            checked = is_checked(task.id)
            gates_ok, gate_output = run_gates(args)

            with log_path.open("a", encoding="utf-8") as log:
                log.write("\n\n=== gates ===\n")
                log.write(gate_output)
                log.write("\n")

            if rc == 0 and gates_ok and not checked and not args.require_checkbox:
                if set_task_checked(task.id, True):
                    checked = True
                    print(
                        "  note: gates passed but the box was unchecked; "
                        "checked by ralph"
                    )

            if rc != 0:
                failure_text = (
                    f"opencode exited with code {rc}.\n\n{gate_output}"
                )
            elif not gates_ok:
                failure_text = gate_output
            elif not checked:
                failure_text = (
                    "The task box is still unchecked. Complete the task and "
                    "check the box."
                )
            else:
                success = True
                break

            state["tasks"][task.id] = {
                "status": "failed",
                "attempts": attempt,
                "log": log_rel,
                "prompt": prompt_rel,
                "finished_at": datetime.now().isoformat(timespec="seconds"),
            }
            save_state(state)

            failure_text = (
                failure_text
                + "\n\nAgent log tail:\n"
                + agent_tail
            ).strip()

            if key_error:
                print_key_hint()
                fatal_key_error = True
                break

            if attempt < args.attempts:
                time.sleep(args.retry_delay)

        if success:
            commit = commit_task(task, args)
            evidence = (
                "gates skipped by --skip-gates"
                if args.skip_gates
                else "cargo fmt --check, cargo clippy, cargo test passed"
            )
            ensure_session_log(task, log_rel, commit, evidence)
            state["tasks"][task.id] = {
                "status": "done",
                "attempts": attempt,
                "commit": commit,
                "log": log_rel,
                "prompt": prompt_rel,
                "finished_at": datetime.now().isoformat(timespec="seconds"),
            }
            save_state(state)
            results.append(
                {
                    "id": task.id,
                    "status": "done",
                    "attempts": attempt,
                    "commit": commit,
                    "log": log_rel,
                }
            )
            print(f"  done{f' ({commit})' if commit else ''}")
        else:
            if is_checked(task.id):
                set_task_checked(task.id, False)
                print("  note: unchecked the box because verification failed")

            state["tasks"][task.id] = {
                "status": "failed",
                "attempts": attempt,
                "log": log_rel,
                "prompt": prompt_rel,
                "finished_at": datetime.now().isoformat(timespec="seconds"),
            }
            save_state(state)
            results.append(
                {
                    "id": task.id,
                    "status": "failed",
                    "attempts": attempt,
                    "commit": None,
                    "log": log_rel,
                }
            )
            failures += 1
            print(
                f"  FAILED after {attempt} attempt(s); see {log_rel}"
            )
            if not args.continue_on_failure or fatal_key_error:
                break

        if args.sleep:
            time.sleep(args.sleep)

    write_summary(run_dir, results)

    if failures:
        print(
            f"ralph: {failures} stage(s) failed; rerun the same command to resume"
        )
        return 1

    print("ralph: all selected stages complete")
    return 0


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="ralph",
        description=(
            "Run one OpenCode session per unchecked task in plan.md, "
            "verifying gates and committing each stage."
        ),
    )
    parser.add_argument(
        "--only",
        action="append",
        metavar="S0.1[,S0.2]",
        help="run only these task ids (repeatable, comma-separated)",
    )
    parser.add_argument("--from", dest="start_at", metavar="S0.1")
    parser.add_argument("--until", dest="stop_at", metavar="S1.6")
    parser.add_argument(
        "--max-stages", type=int, default=0, help="stop after N tasks"
    )
    parser.add_argument("--list", action="store_true", help="list tasks and exit")
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="print the command and first prompt without running",
    )
    parser.add_argument(
        "--attempts",
        type=int,
        default=2,
        help="attempts per task, each a fresh session (default 2)",
    )
    parser.add_argument(
        "--retry-delay", type=float, default=10, help="seconds between attempts"
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=2700,
        help="per-attempt timeout in seconds (default 2700)",
    )
    parser.add_argument(
        "--gate-timeout",
        type=int,
        default=1800,
        help="per-gate timeout in seconds (default 1800)",
    )
    parser.add_argument(
        "--sleep", type=float, default=2, help="seconds between tasks"
    )
    parser.add_argument(
        "--model",
        default=os.environ.get("RALPH_MODEL"),
        help="provider/model[#variant] passed to opencode",
    )
    parser.add_argument(
        "--agent",
        default=os.environ.get("RALPH_AGENT", "build"),
        help="opencode agent (default build)",
    )
    parser.add_argument(
        "--standalone",
        action="store_true",
        help="run opencode with a private server",
    )
    parser.add_argument(
        "--debug",
        action="store_true",
        help="pass --print-logs --log-level debug to opencode",
    )
    parser.add_argument(
        "--no-commit",
        action="store_true",
        help="do not git commit after each stage",
    )
    parser.add_argument(
        "--continue-on-failure",
        action="store_true",
        help="continue to later tasks after a failure",
    )
    parser.add_argument(
        "--require-checkbox",
        action="store_true",
        help="fail unless the agent checks the box (disables auto-check)",
    )
    parser.add_argument(
        "--skip-gates",
        action="store_true",
        help="verify only the plan checkbox (debugging)",
    )
    parser.add_argument(
        "--rerun",
        action="store_true",
        help="run selected tasks even if already marked done",
    )
    parser.add_argument(
        "--extra",
        default="",
        help="extra instructions appended to every prompt",
    )
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    if args.attempts < 1:
        die("--attempts must be >= 1", 2)
    if args.max_stages < 0:
        die("--max-stages must be >= 0", 2)

    try:
        return run(args)
    except KeyboardInterrupt:
        if CURRENT_PROC is not None:
            terminate(CURRENT_PROC)
        print("\nralph: interrupted", file=sys.stderr)
        return 130


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
