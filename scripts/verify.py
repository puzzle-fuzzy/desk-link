#!/usr/bin/env python3
"""Run DeskLink's local verification layers on Windows and macOS (Python 3.9+)."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import subprocess
import sys
import time
from typing import Optional


ROOT = Path(__file__).resolve().parents[1]
PROJECTS = {"frontend": "apps/windows-ui", "account": "server/account", "diagnostics": "server/diagnostics"}
LAYERS = ("fast", "integration", "native", "perf")


@dataclass(frozen=True)
class Task:
    id: str
    layer: str
    command: tuple[str, ...]
    cwd: str = "."
    requires: tuple[str, ...] = ()
    host: Optional[str] = None
    device: bool = False
    scope: str = "local automated checks; no real two-device acceptance"
    files: tuple[str, ...] = ()
    tools: tuple[str, ...] = ()


class CleanupFailure(OSError):
    """Do not launch more tasks while the child process tree may still run."""


def catalog(target: Path, soak_seconds: int) -> list[Task]:
    tasks = []
    for name, directory in PROJECTS.items():
        tasks.append(Task(f"{name}-deps", "setup", ("bun", "install", "--frozen-lockfile"), directory))
        tasks.append(Task(f"{name}-types", "fast", ("bun", "run", "tsc", "--noEmit"), directory, (f"{name}-deps",)))
        tasks.append(Task(f"{name}-tests", "fast", ("bun", "test"), directory, (f"{name}-deps",)))
    tasks.extend([
        Task("python-tests", "fast", ("python", "-m", "unittest", "discover", "-s", "scripts/tests", "-p", "test_*.py")),
        Task("rust-format", "fast", ("cargo", "fmt", "--all", "--", "--check")),
        Task("rust-unit", "fast", ("cargo", "test", "--locked", "--workspace", "--lib")),
        Task("frontend-build", "integration", ("bun", "run", "build"), PROJECTS["frontend"], ("frontend-deps",)),
        Task("rust-clippy", "integration", ("cargo", "clippy", "--locked", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"), requires=("frontend-build",)),
        Task("rust-contracts", "integration", ("cargo", "test", "--locked", "-p", "desklink-protocol", "-p", "desklink-video", "-p", "desklink-session", "-p", "desklink-crypto", "--tests")),
        Task("transport-localhost", "integration", ("cargo", "test", "--locked", "-p", "desklink-transport", "--test", "localhost")),
        Task("relay-localhost", "integration", ("cargo", "test", "--locked", "-p", "desklink-relay", "--test", "session")),
        Task("ffi-runtime", "integration", ("cargo", "test", "--locked", "-p", "desklink-ffi", "--tests")),
        Task("media-recovery", "integration", ("cargo", "test", "--locked", "-p", "desklink-end-to-end", "--test", "recovery")),
        Task("windows-contracts", "integration", ("cargo", "test", "--locked", "-p", "desklink-windows", "--tests"), host="win32"),
        Task("apple-ffi-build", "native", ("cargo", "build", "--locked", "--release", "-p", "desklink-ffi", "--target", "aarch64-apple-darwin"), host="darwin", scope="Apple Silicon FFI build; no permissions/device acceptance"),
        Task("apple-core", "native", ("swift", "test", "--arch", "arm64", "-Xlinker", "-L" + str(target / "aarch64-apple-darwin/release")), "apps/apple", ("apple-ffi-build",), "darwin", scope="Swift/FFI tests; no actual AppKit IME/permission acceptance"),
        Task("macos-app", "native", ("swift", "test", "--arch", "arm64", "-Xlinker", "-L" + str(target / "aarch64-apple-darwin/release")), "apps/macos", ("apple-ffi-build",), "darwin", True, "macOS tests may access native input/encoder APIs; run only on a dedicated device"),
        Task("windows-capture", "native", ("cargo", "test", "--locked", "-p", "desklink-windows", "--test", "capture_smoke", "--", "--ignored", "--nocapture"), host="win32", device=True, scope="interactive desktop capture; not two-device acceptance"),
        Task("windows-encode", "native", ("cargo", "test", "--locked", "-p", "desklink-windows", "--test", "encoder_smoke", "captured_desktop_frame_encodes_to_h264", "--", "--ignored", "--exact", "--nocapture"), host="win32", device=True, scope="one native captured H.264 frame; no sustained latency measurement"),
        Task("local-media-soak", "perf", ("cargo", "test", "--locked", "-p", "desklink-windows", "--test", "runtime_smoke", "windows::local_relay_hardware_soak_keeps_secure_media_and_cursor_alive", "--", "--ignored", "--exact", "--nocapture"), host="win32", device=True, scope=f"{soak_seconds}s local hardware/encrypted media soak; not a performance baseline or WAN test"),
        Task("offline-media-baseline", "perf", ("python", "scripts/benchmark-windows-media.py", "--width", "1920", "--height", "1080", "--frames", "120", "--warmup", "10", "--output", "dist/verify/media-baseline.json"), host="win32", scope="DL2-014 offline synthetic conversion only; requires integration of windows-media commit 254f8be; no capture or input", files=("scripts/benchmark-windows-media.py", "apps/windows/examples/media_baseline.rs"), tools=("cargo",)),
        Task("performance-baseline", "perf", (), scope="DL2-014 full same-hardware before/after latency, copy, CPU/GPU and memory acceptance remains unavailable in this checkout"),
    ])
    return tasks


def select_tasks(tasks: list[Task], layers: list[str], selected: list[str]) -> list[Task]:
    by_id = {task.id: task for task in tasks}
    unknown = set(selected) - by_id.keys()
    if unknown:
        raise ValueError("unknown tasks: " + ", ".join(sorted(unknown)))
    result: list[Task] = []
    visiting: set[str] = set()
    visited: set[str] = set()

    def add(task: Task) -> None:
        if task.id in visiting:
            raise ValueError("cyclic task dependencies: " + task.id)
        if task.id in visited:
            return
        visiting.add(task.id)
        for dependency in task.requires:
            add(by_id[dependency])
        visiting.remove(task.id)
        visited.add(task.id)
        result.append(task)

    for task in tasks:
        included = task.id in selected if selected else task.layer in layers
        if included:
            add(task)
    return result


def skip_reason(task: Task, host: str, allow_device: bool, root: Path = ROOT) -> Optional[str]:
    if task.host and task.host != host:
        return f"requires {task.host}; current host is {host}"
    if task.host == "darwin" and platform.machine().lower() not in ("arm64", "aarch64"):
        return "requires Apple Silicon arm64 and Xcode/Swift"
    if task.device and not allow_device:
        return "requires a dedicated interactive device and explicit --allow-device"
    missing = [name for name in task.files if not (root / name).is_file()]
    if missing:
        return "requires integrated source files: " + ", ".join(missing)
    if not task.command:
        return task.scope
    return None


def capture(command: list[str], root: Path) -> str:
    completed = subprocess.run(command, cwd=root, check=True, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=30)
    return completed.stdout.strip()


def metadata(root: Path) -> dict:
    return {
        "source_commit": capture(["git", "rev-parse", "HEAD"], root),
        "source_dirty": bool(capture(["git", "status", "--porcelain", "--untracked-files=all"], root)),
        "platform": sys.platform,
        "machine": platform.machine(),
        "python": sys.version,
        "rust_toolchain": (root / "rust-toolchain.toml").read_text(encoding="utf-8"),
    }


def tool_versions(tasks: list[Task], bun: str, root: Path, allow_device: bool) -> tuple[dict, dict]:
    active = set()
    for task in tasks:
        if task.command and not skip_reason(task, sys.platform, allow_device, root):
            active.update((task.command[0], *task.tools))
    versions, errors = {}, {}
    if "bun" in active:
        pins = {json.loads((root / directory / "package.json").read_text(encoding="utf-8"))["packageManager"] for directory in PROJECTS.values()}
        try:
            actual = capture([bun, "--version"], root)
            versions["bun"] = actual
            if pins != {"bun@1.3.14"} or actual != "1.3.14":
                errors["bun"] = f"Bun 1.3.14 required; manifests={sorted(pins)}, executable={actual}"
        except (OSError, subprocess.SubprocessError) as error:
            errors["bun"] = str(error)
    if "cargo" in active:
        try:
            versions["cargo"] = capture(["cargo", "--version"], root)
            versions["rustc"] = capture(["rustc", "--version"], root)
            pin = re.search(r'^channel\s*=\s*"([^"]+)"', (root / "rust-toolchain.toml").read_text(encoding="utf-8"), re.MULTILINE)
            if not pin or not versions["rustc"].startswith(f"rustc {pin.group(1)} "):
                errors["cargo"] = "rustc must match rust-toolchain.toml (remove any toolchain override)"
        except (OSError, subprocess.SubprocessError) as error:
            errors["cargo"] = str(error)
    if "swift" in active:
        try:
            versions["swift"] = capture(["swift", "--version"], root)
        except (OSError, subprocess.SubprocessError) as error:
            errors["swift"] = str(error)
    return versions, errors


def command_for(task: Task, root: Path, bun: str) -> list[str]:
    command = list(task.command)
    if not command:
        return command
    if command[0] == "python":
        command[0] = sys.executable
    elif command[0] == "bun":
        command[0] = bun
    elif command[0] == "cargo" and sys.platform == "win32":
        command = [sys.executable, str(root / "scripts/run-windows-cargo.py"), *command[1:]]
    return command


def write_report(path: Path, report: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    temporary.replace(path)


def stop_process_tree(process: subprocess.Popen) -> None:
    """Stop wrappers and their children before proceeding to another task."""
    try:
        if sys.platform == "win32":
            completed = subprocess.run(
                ["taskkill", "/PID", str(process.pid), "/T", "/F"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
                timeout=15,
            )
            if completed.returncode and process.poll() is None:
                raise OSError(f"taskkill failed for process tree {process.pid}")
        else:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
    finally:
        if process.poll() is None:
            process.kill()
        process.wait(timeout=15)


def execute(command: list[str], cwd: Path, environment: dict[str, str], output, timeout: int) -> int:
    options = {"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP} if sys.platform == "win32" else {"start_new_session": True}
    process = subprocess.Popen(command, cwd=cwd, env=environment, stdout=output, stderr=subprocess.STDOUT, **options)
    try:
        return process.wait(timeout=timeout)
    except (subprocess.TimeoutExpired, KeyboardInterrupt):
        try:
            stop_process_tree(process)
        except (OSError, subprocess.SubprocessError) as error:
            raise CleanupFailure(f"process tree cleanup failed; inspect children: {error}") from error
        raise


def test_count(task: Task, output: str) -> Optional[int]:
    """A zero-match test runner must never provide successful verification."""
    command = task.command
    pattern = None
    if command[:2] == ("bun", "test"):
        pattern = r"(?m)^\s*(\d+) pass\b"
    elif command[:2] == ("cargo", "test"):
        pattern = r"test result: ok\. (\d+) passed;"
    elif command[:2] == ("swift", "test"):
        pattern = r"(?:Executed|Test run with) (\d+) tests?\b"
    elif command[:2] == ("python", "-m") and "unittest" in command:
        pattern = r"(?m)^Ran (\d+) tests?\b"
    if pattern is None:
        return None
    counts = [int(count) for count in re.findall(pattern, output)]
    return max(counts, default=0) if command[0] == "swift" else sum(counts)


def run_tasks(tasks: list[Task], *, root: Path, bun: str, report_path: Path, report: dict, environment: dict[str, str], allow_device: bool, timeout: int, errors: dict) -> int:
    log_dir = report_path.parent / (report_path.stem + "-logs")
    log_dir.mkdir(parents=True, exist_ok=True)
    results: dict[str, str] = {}
    abort_reason = None
    report["checks"] = []
    report["status"] = "running"
    report["passed"] = False
    write_report(report_path, report)
    for task in tasks:
        command = command_for(task, root, bun)
        record = {"id": task.id, "layer": task.layer, "command": command, "cwd": str(root / task.cwd), "scope": task.scope}
        reason = skip_reason(task, sys.platform, allow_device, root)
        if abort_reason:
            record.update(status="blocked", reason=abort_reason)
        elif reason:
            record.update(status="skipped", reason=reason)
        elif any(results.get(dependency) != "passed" for dependency in task.requires):
            record.update(status="blocked", reason="prerequisite did not pass: " + ", ".join(task.requires))
        elif any(tool in errors for tool in (task.command[0], *task.tools)):
            record.update(status="failed", reason="; ".join(errors[tool] for tool in (task.command[0], *task.tools) if tool in errors))
        else:
            log = log_dir / (task.id + ".log")
            record["log"] = str(log)
            started = time.monotonic()
            print(f"[{task.layer}] {task.id}: {subprocess.list2cmdline(command)}", flush=True)
            try:
                with log.open("w", encoding="utf-8") as output:
                    exit_code = execute(command, root / task.cwd, environment, output, timeout)
                record.update(status="passed" if exit_code == 0 else "failed", exit_code=exit_code)
                if exit_code == 0:
                    count = test_count(task, log.read_text(encoding="utf-8", errors="replace"))
                    if count is not None:
                        record["tests_reported"] = count
                        if count == 0:
                            record.update(status="failed", reason="test runner reported no executed/passing tests; inspect the filter and log")
            except CleanupFailure as error:
                abort_reason = str(error)
                record.update(status="failed", reason=abort_reason)
            except (OSError, subprocess.TimeoutExpired) as error:
                record.update(status="failed", reason=str(error))
            except KeyboardInterrupt:
                record.update(status="failed", reason="interrupted by user")
                report["checks"].append(record)
                report["status"] = "interrupted"
                write_report(report_path, report)
                return 130
            record["elapsed_seconds"] = round(time.monotonic() - started, 3)
            if record["status"] == "failed" and log.exists():
                print(log.read_text(encoding="utf-8", errors="replace")[-12000:], flush=True)
        results[task.id] = record["status"]
        report["checks"].append(record)
        print(f"{task.id}: {record['status']}" + (f" ({record['reason']})" if "reason" in record else ""), flush=True)
        write_report(report_path, report)
    statuses = set(results.values())
    report["status"] = "failed" if "failed" in statuses else "incomplete" if statuses != {"passed"} else "passed"
    report["passed"] = report["status"] == "passed"
    report["completed_at_unix_s"] = int(time.time())
    write_report(report_path, report)
    print(f"Verification {report['status']}: {report_path}", flush=True)
    return {"passed": 0, "failed": 1, "incomplete": 2}[report["status"]]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("layers", nargs="*", help="fast (default), integration, native, perf, all")
    parser.add_argument("--task", action="append", default=[], help="select an exact task ID (repeatable), including prerequisites")
    parser.add_argument("--list", action="store_true", help="print the plan; do not install or run anything")
    parser.add_argument("--bun", default="bun", help="Bun executable; must report 1.3.14")
    parser.add_argument("--target-dir", type=Path, help="isolated Cargo target directory; otherwise honor CARGO_TARGET_DIR")
    parser.add_argument("--report", type=Path, default=ROOT / "dist/verify/report.json")
    parser.add_argument("--allow-device", action="store_true", help="opt in to interactive capture/native APIs on a dedicated device")
    parser.add_argument("--soak-seconds", type=int, default=30, help="local hardware soak, 10..300 seconds")
    parser.add_argument("--timeout", type=int, default=1800, help="timeout for each subprocess in seconds")
    args = parser.parse_args()
    args.bun = shutil.which(args.bun) or args.bun
    if set(args.layers) - {*LAYERS, "all"}:
        parser.error("unknown layer; choose fast, integration, native, perf, all")
    if args.layers and args.task:
        parser.error("select layers or --task, not both")
    if not 10 <= args.soak_seconds <= 300 or args.timeout < 1:
        parser.error("soak must be 10..300 seconds and timeout must be positive")
    target = (args.target_dir or Path(os.environ.get("CARGO_TARGET_DIR", str(ROOT / "target")))).resolve()
    layers = list(LAYERS) if "all" in args.layers else args.layers or ["fast"]
    try:
        tasks = select_tasks(catalog(target, args.soak_seconds), layers, args.task)
    except ValueError as error:
        parser.error(str(error))
    if args.list:
        for task in tasks:
            reason = skip_reason(task, sys.platform, args.allow_device)
            print(f"{task.id:24} {task.layer:12} " + ("SKIP: " + reason if reason else subprocess.list2cmdline(command_for(task, ROOT, args.bun))))
        return 0
    environment = os.environ.copy()
    environment["CARGO_TARGET_DIR"] = str(target)
    environment["DESKLINK_SOAK_SECONDS"] = str(args.soak_seconds)
    if Path(args.bun).is_file():
        environment["PATH"] = str(Path(args.bun).resolve().parent) + os.pathsep + environment.get("PATH", "")
    report = {"schema": 1, "started_at_unix_s": int(time.time()), "selection": args.task or layers, "target_dir": str(target), "allow_device": args.allow_device, "soak_seconds": args.soak_seconds, "timeout_seconds": args.timeout}
    try:
        report.update(metadata(ROOT))
        versions, errors = tool_versions(tasks, args.bun, ROOT, args.allow_device)
        report["tools"] = versions
        report["tool_errors"] = errors
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        report.update(status="failed", passed=False, reason=str(error))
        write_report(args.report.resolve(), report)
        print(f"Verification preflight failed: {error}", file=sys.stderr)
        return 1
    return run_tasks(tasks, root=ROOT, bun=args.bun, report_path=args.report.resolve(), report=report, environment=environment, allow_device=args.allow_device, timeout=args.timeout, errors=errors)


if __name__ == "__main__":
    raise SystemExit(main())
