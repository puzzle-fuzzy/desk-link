#!/usr/bin/env python3
"""Record local synthetic GPU capabilities or stages without desktop capture."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]


def source_identity() -> dict[str, object]:
    spec = importlib.util.spec_from_file_location(
        "desklink_media_baseline", ROOT / "scripts/benchmark-windows-media.py"
    )
    assert spec and spec.loader
    baseline = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(baseline)
    return baseline.source_identity(ROOT)


def hardware_inventory() -> dict[str, object]:
    inventory: dict[str, object] = {}
    # Deliberately omit computer names, account names, PNP instance IDs and UUIDs.
    command = (
        "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new(); "
        "@(Get-CimInstance Win32_VideoController | ForEach-Object { "
        "[pscustomobject]@{name=$_.Name;driver_version=$_.DriverVersion;"
        "driver_date_utc=$(if($_.DriverDate){$_.DriverDate.ToUniversalTime().ToString('o')}else{$null});"
        "adapter_ram_wmi32=$_.AdapterRAM} }) | ConvertTo-Json -Compress"
    )
    try:
        result = subprocess.run(
            ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", command],
            capture_output=True, text=True, encoding="utf-8", timeout=20, check=False,
        )
        inventory["wmi"] = (
            {"status": "ok", "controllers": json.loads(result.stdout),
             "memory_note": "32-bit WMI AdapterRAM may truncate; do not infer physical VRAM"}
            if result.returncode == 0 else {"status": "failed", "exit_code": result.returncode}
        )
    except (OSError, subprocess.TimeoutExpired, ValueError) as error:
        inventory["wmi"] = {"status": "unavailable", "reason": type(error).__name__}
    executable = shutil.which("nvidia-smi")
    if executable:
        try:
            result = subprocess.run(
                [executable, "--query-gpu=name,driver_version,memory.total",
                 "--format=csv,noheader,nounits"],
                capture_output=True, text=True, encoding="utf-8", timeout=15, check=False,
            )
            inventory["nvidia_smi"] = (
                {"status": "ok", "columns": ["name", "driver_version", "memory_total_mib"],
                 "rows": [[cell.strip() for cell in row.split(",")] for row in result.stdout.splitlines() if row.strip()]}
                if result.returncode == 0 else {"status": "failed", "exit_code": result.returncode}
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            inventory["nvidia_smi"] = {"status": "unavailable", "reason": type(error).__name__}
    else:
        inventory["nvidia_smi"] = {"status": "unavailable", "reason": "not_installed"}
    return inventory


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=["capability", "benchmark"], default="capability")
    parser.add_argument("--width", type=int, default=1280)
    parser.add_argument("--height", type=int, default=720)
    parser.add_argument("--output-width", type=int)
    parser.add_argument("--output-height", type=int)
    parser.add_argument("--frames", type=int, default=30)
    parser.add_argument("--warmup", type=int, default=3)
    parser.add_argument("--repeat", type=int)
    parser.add_argument("--adapter-index", type=int)
    parser.add_argument("--encode", action="store_true", help="benchmark synthetic GPU surface encoding")
    parser.add_argument("--input-path", choices=["gpu", "cpu"], default="gpu", help="cpu feeds synthetic owned BGRA to the existing encoder; backend is reported")
    parser.add_argument("--experimental-4k", action="store_true", help="explicit High/40.5Mbps/3840x2160 encoder maximum; requires --encode")
    parser.add_argument("--motion", action="store_true", help="change the synthetic fixture each frame")
    parser.add_argument("--workload", choices=["desktop", "tiles"], default="desktop", help="deterministic desktop-like glyph/gradient/panels or simple validation tiles")
    parser.add_argument("--profile", choices=["release", "dev"], default="release")
    parser.add_argument("--label", default="synthetic-gpu")
    parser.add_argument("--target-dir", type=Path, help="otherwise use explicit CARGO_TARGET_DIR")
    parser.add_argument("--output", type=Path, default=ROOT / "dist/windows/gpu-media-probe.json")
    args = parser.parse_args()
    if platform.system() != "Windows":
        parser.error("the D3D11/MFT GPU probe requires Windows")
    if (args.output_width is None) != (args.output_height is None):
        parser.error("--output-width and --output-height must be supplied together")
    dimensions = [args.width, args.height]
    if args.output_width is not None:
        dimensions.extend([args.output_width, args.output_height])
    if any(value < 2 or value > 8192 or value % 2 for value in dimensions):
        parser.error("dimensions must be even and within 2..8192")
    repeats = args.repeat if args.repeat is not None else (3 if args.mode == "benchmark" else 1)
    if not 1 <= repeats <= 20 or not 1 <= args.frames <= 10_000 or not 0 <= args.warmup <= 1000:
        parser.error("repeat 1..20, frames 1..10000 and warmup 0..1000 are required")
    if args.adapter_index is not None and args.adapter_index < 0:
        parser.error("--adapter-index must be nonnegative")
    if args.encode and args.mode != "benchmark":
        parser.error("--encode requires --mode benchmark")
    if (args.input_path == "cpu" or args.experimental_4k) and not args.encode:
        parser.error("--input-path cpu and --experimental-4k require --encode")
    environment = os.environ.copy()
    if args.target_dir:
        environment["CARGO_TARGET_DIR"] = str(args.target_dir.resolve())
    if not environment.get("CARGO_TARGET_DIR"):
        parser.error("set CARGO_TARGET_DIR or --target-dir to an isolated native build directory")
    command = [
        sys.executable, "scripts/run-windows-cargo.py", "run", "--locked",
        "--profile", args.profile, "-p", "desklink-windows", "--example", "gpu_media_probe", "--",
        "--mode", args.mode, "--width", str(args.width), "--height", str(args.height),
        "--frames", str(args.frames), "--warmup", str(args.warmup),
        "--input-path", args.input_path,
        "--workload", args.workload,
    ]
    if args.output_width is not None:
        command.extend(["--output-width", str(args.output_width), "--output-height", str(args.output_height)])
    if args.adapter_index is not None:
        command.extend(["--adapter-index", str(args.adapter_index)])
    if args.encode:
        command.append("--encode")
    if args.motion:
        command.append("--motion")
    if args.experimental_4k:
        command.append("--experimental-4k")
    report: dict[str, object] = {
        "schema_version": 1,
        "scope": "offline_synthetic_gpu_capabilities_and_local_stages",
        "network": "none", "desktop_capture": False, "input_injection": False,
        "started_utc": datetime.now(timezone.utc).isoformat(),
        "environment": {"os": platform.system(), "release": platform.release(),
                        "machine": platform.machine(), "label": args.label},
        "hardware_inventory": hardware_inventory(), "build_profile": args.profile,
        "command": ["python", *command[1:]], "runs": [],
        "measurement_limits": [
            "CPU submission duration is not completed GPU execution time",
            "GPU timestamp duration requires valid non-disjoint timestamps and supported query coverage",
            "Validation readback and synthetic CPU upload are separate from production-path copy counters",
            "Known texture/view creation counts are not process-wide or driver allocation counts",
            "MFT D3D11_AWARE alone does not prove manager acceptance, surface input, or encoded output",
            "No network, desktop capture, decode/display, or input-to-display latency is measured",
            "Returned output counts describe measurement calls, not a drained input cohort or real-time FPS",
            "Same source/settings permit a local stage comparison, not decoded quality equivalence or typical desktop benefit",
        ],
    }
    success = True
    for index in range(repeats):
        before = source_identity()
        started = time.monotonic_ns()
        result = subprocess.run(command, cwd=ROOT, env=environment, capture_output=True, text=True, encoding="utf-8", check=False)
        duration_us = (time.monotonic_ns() - started) // 1000
        after = source_identity()
        run: dict[str, object] = {
            "index": index + 1, "source_before": before, "source_after": after,
            "source_unchanged_during_run": before == after,
            "process_elapsed_us": duration_us, "exit_code": result.returncode,
            "stderr_sha256": hashlib.sha256(result.stderr.encode("utf-8")).hexdigest(),
        }
        try:
            run["probe"] = json.loads(result.stdout)
        except ValueError:
            run["probe"] = None
            run["failure_stage"] = "missing_or_invalid_probe_json"
            success = False
        if args.mode == "benchmark":
            probe = run["probe"]
            cases = probe.get("cases", []) if isinstance(probe, dict) else []
            run["benchmark_validated"] = bool(cases) and all(
                case.get("result", {}).get("status") == "ok"
                and case.get("result", {}).get("validation", {}).get("passed") is True
                for case in cases
            )
            run["requested_gpu_surface_path_confirmed"] = (
                all(case.get("result", {}).get("gpu_surface_encoding_confirmed") is True for case in cases)
                if args.encode and args.input_path == "gpu" else None
            )
            run["stage_comparison_signature"] = [
                {key: case.get("result", {}).get(key) for key in (
                    "input_path", "benchmark_workload", "benchmark_motion", "fixture_initial_blake2s256",
                    "output_dimensions", "profile", "actual_bitrate", "requested_encoder_settings", "backend",
                )} | {"adapter_index": case.get("adapter_index")}
                for case in cases
            ]
            if not run["benchmark_validated"]:
                run.setdefault("failure_stage", "benchmark_validation_or_adapter_failure")
                success = False
        if result.returncode:
            success = False
            print(result.stderr, file=sys.stderr)
        report["runs"].append(run)
    runs = report["runs"]
    report["all_runs_individually_source_unchanged"] = all(run["source_unchanged_during_run"] for run in runs)
    first_source = runs[0]["source_before"]
    report["source_unchanged_during_all_runs"] = all(
        run["source_before"] == first_source and run["source_after"] == first_source for run in runs
    )
    report["all_runs_source_clean"] = all(
        run["source_before"].get("dirty") is False and run["source_after"].get("dirty") is False
        for run in runs
    )
    report["performance_comparison_eligible"] = (
        args.mode == "benchmark" and args.profile == "release" and success
        and report["source_unchanged_during_all_runs"] and report["all_runs_source_clean"]
        and all(run.get("requested_gpu_surface_path_confirmed") is not False for run in runs)
        and all(run.get("stage_comparison_signature") == runs[0].get("stage_comparison_signature") for run in runs)
    )
    report["comparison_scope"] = "repeat_consistency_for_local_stages_only_not_decoded_quality_or_end_to_end_benefit"
    report["quality_equivalence_verified"] = False
    report["finished_utc"] = datetime.now(timezone.utc).isoformat()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"Saved synthetic GPU probe to {args.output}")
    return 0 if success else 1


if __name__ == "__main__":
    raise SystemExit(main())
