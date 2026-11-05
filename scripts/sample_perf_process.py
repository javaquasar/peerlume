#!/usr/bin/env python3
"""Sample only one spawned, network-free perf-workload process."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time


def proc_memory(text: str) -> tuple[int | None, int | None]:
    values = {}
    for line in text.splitlines():
        key, separator, value = line.partition(":")
        if separator and key in {"VmRSS", "VmHWM"}:
            fields = value.split()
            if len(fields) == 2 and fields[1] == "kB":
                values[key] = int(fields[0]) * 1024
    return values.get("VmRSS"), values.get("VmHWM")


class WindowsProbe:
    def __init__(self, process: subprocess.Popen):
        import ctypes
        from ctypes import wintypes

        self.ctypes = ctypes
        self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        self.psapi = ctypes.WinDLL("psapi", use_last_error=True)

        class MemoryCounters(ctypes.Structure):
            _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [
                (name, ctypes.c_size_t)
                for name in (
                    "PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage",
                    "QuotaPagedPoolUsage", "QuotaPeakNonPagedPoolUsage",
                    "QuotaNonPagedPoolUsage", "PagefileUsage", "PeakPagefileUsage",
                )
            ]

        self.MemoryCounters = MemoryCounters
        self.FileTime = wintypes.FILETIME
        self.kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        self.kernel.OpenProcess.restype = wintypes.HANDLE
        self.kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        self.kernel.CloseHandle.restype = wintypes.BOOL
        self.kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
        self.kernel.GetProcessTimes.restype = wintypes.BOOL
        self.psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(MemoryCounters), wintypes.DWORD]
        self.psapi.GetProcessMemoryInfo.restype = wintypes.BOOL
        self.handle = self.kernel.OpenProcess(0x0400 | 0x0010, False, process.pid)
        if not self.handle:
            raise OSError(ctypes.get_last_error(), "Cannot open the spawned workload for resource queries")

    def memory(self) -> tuple[int, int]:
        counters = self.MemoryCounters()
        counters.cb = self.ctypes.sizeof(counters)
        if not self.psapi.GetProcessMemoryInfo(self.handle, self.ctypes.byref(counters), counters.cb):
            raise OSError(self.ctypes.get_last_error(), "GetProcessMemoryInfo failed")
        return counters.WorkingSetSize, counters.PeakWorkingSetSize

    def final_cpu(self) -> tuple[int, int, str]:
        values = [self.FileTime() for _ in range(4)]
        if not self.kernel.GetProcessTimes(self.handle, *(self.ctypes.byref(value) for value in values)):
            raise OSError(self.ctypes.get_last_error(), "GetProcessTimes failed")
        ns = lambda value: ((int(value.dwHighDateTime) << 32) | int(value.dwLowDateTime)) * 100
        return ns(values[3]), ns(values[2]), "Windows GetProcessTimes (completed child handle)"

    def close(self):
        self.kernel.CloseHandle(self.handle)


class LinuxProbe:
    def __init__(self, process: subprocess.Popen):
        import resource

        self.resource = resource
        self.pid = process.pid
        self.before = resource.getrusage(resource.RUSAGE_CHILDREN)

    def memory(self) -> tuple[int | None, int | None]:
        return proc_memory(Path(f"/proc/{self.pid}/status").read_text())

    def final_cpu(self) -> tuple[int, int, str]:
        after = self.resource.getrusage(self.resource.RUSAGE_CHILDREN)
        return (
            round((after.ru_utime - self.before.ru_utime) * 1_000_000_000),
            round((after.ru_stime - self.before.ru_stime) * 1_000_000_000),
            "Linux resource.getrusage(RUSAGE_CHILDREN) delta; one child only",
        )

    def close(self):
        pass


class SampleSummary:
    def __init__(self):
        self.count = 0
        self.first = None
        self.last = None
        self.max_gap = 0
        self.rss_peak = None
        self.rss_peak_at = None
        self.observed_os_peak = None

    def observe(self, offset: int, rss: int | None, os_peak: int | None):
        if rss is None:
            return
        self.count += 1
        if self.first is None:
            self.first = offset
        if self.last is not None:
            self.max_gap = max(self.max_gap, offset - self.last)
        self.last = offset
        if self.rss_peak is None or rss > self.rss_peak:
            self.rss_peak, self.rss_peak_at = rss, offset
        if os_peak is not None:
            self.observed_os_peak = max(self.observed_os_peak or 0, os_peak)

    def report(self, wall_ns: int, interval_ns: int) -> dict:
        return {
            "successful_samples": self.count, "requested_interval_ns": interval_ns,
            "first_sample_offset_ns": self.first, "last_sample_offset_ns": self.last,
            "max_successful_sample_gap_ns": self.max_gap if self.count > 1 else None,
            "unsampled_start_ns": self.first,
            "unsampled_tail_ns": None if self.last is None else max(0, wall_ns - self.last),
            "sampled_rss_peak_bytes": self.rss_peak, "sampled_rss_peak_offset_ns": self.rss_peak_at,
            "os_high_water_mark_observed_bytes": self.observed_os_peak,
        }


def verify_workload(report: dict, repetitions: int, source_label: str):
    if report.get("completed") is not True or report.get("workload") != "synthetic_frontend_and_checkpoint":
        raise ValueError("Child did not complete the expected synthetic workload")
    if report.get("source_label") != source_label or [case["torrents"] for case in report["cases"]] != [1, 10, 100]:
        raise ValueError("Child workload identity or torrent counts differ from the request")
    for case in report["cases"]:
        if case["files_per_torrent"] != 100 or case["view"]["repetitions"] != repetitions:
            raise ValueError("Child view inputs differ from the requested workload")
        if any(case[key]["repetitions"] != min(repetitions, 20) for key in ("checkpoint_unchanged", "checkpoint_changed")):
            raise ValueError("Child checkpoint repetitions differ from the request")
    if report["large_model"]["files"] != 100_000 or report["large_model"]["cached_search"]["repetitions"] != repetitions:
        raise ValueError("Child large-file workload differs from the request")


def main(arguments=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--executable", required=True, type=Path)
    parser.add_argument("--workload-report", required=True, type=Path)
    parser.add_argument("--report", required=True, type=Path)
    parser.add_argument("--source-label", required=True)
    parser.add_argument("--repetitions", type=int, default=200)
    parser.add_argument("--sample-ms", type=float, default=10.0)
    parser.add_argument("--timeout-seconds", type=float, default=120.0)
    options = parser.parse_args(arguments)
    if not (1 <= options.repetitions <= 10_000 and 1 <= options.sample_ms <= 1000 and 1 <= options.timeout_seconds <= 600):
        parser.error("repetitions must be 1..10000, sample-ms 1..1000 and timeout-seconds 1..600")
    if os.name != "nt" and not sys.platform.startswith("linux"):
        parser.error("resource sampling currently supports Windows and Linux")
    if options.workload_report.exists() or options.report.exists():
        parser.error("evidence output paths must be new")
    if options.workload_report.resolve() == options.report.resolve():
        parser.error("workload and resource reports must use different paths")
    executable = options.executable.resolve(strict=True)
    executable_sha256 = hashlib.sha256(executable.read_bytes()).hexdigest()
    command = [str(executable), "--output", str(options.workload_report.resolve()), "--source-label", options.source_label, "--repetitions", str(options.repetitions)]
    samples = SampleSummary()
    failures = 0
    last_error = None
    user_ns = kernel_ns = None
    cpu_method = None
    timed_out = False
    probe = None
    # Reserve the new resource path before spawning; a crash leaves incomplete
    # evidence rather than overwriting an earlier successful report.
    with options.report.open("x", encoding="utf-8") as report_file:
        started = time.perf_counter_ns()
        with tempfile_stderr() as error_file:
            process = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=error_file, creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
            try:
                probe = WindowsProbe(process) if os.name == "nt" else LinuxProbe(process)
                while True:
                    try:
                        rss, os_peak = probe.memory()
                        samples.observe(time.perf_counter_ns() - started, rss, os_peak)
                    except (OSError, ValueError) as error:
                        failures += 1
                        last_error = str(error)
                    if process.poll() is not None:
                        break
                    if time.perf_counter_ns() - started > options.timeout_seconds * 1_000_000_000:
                        timed_out = True
                        process.kill()  # Only this sampler's synthetic child.
                        break
                    time.sleep(options.sample_ms / 1000)
                process.wait()
                user_ns, kernel_ns, cpu_method = probe.final_cpu()
            except OSError as error:
                last_error = str(error)
                failures += 1
                if process.poll() is None:
                    process.kill()
                    process.wait()
            finally:
                if probe is not None:
                    probe.close()
            wall_ns = time.perf_counter_ns() - started
        child = None
        verification_error = None
        try:
            child = json.loads(options.workload_report.read_text(encoding="utf-8"))
            verify_workload(child, options.repetitions, options.source_label)
        except (OSError, ValueError, KeyError, TypeError) as error:
            verification_error = str(error)
        completed = process.returncode == 0 and not timed_out and verification_error is None and user_ns is not None and samples.count > 0
        cpu_ns = None if user_ns is None else user_ns + kernel_ns
        report = {
            "schema_version": 1, "workload": "synthetic_process_resources", "completed": completed,
            "source_label": options.source_label, "executable_file_name": executable.name,
            "executable_sha256": executable_sha256, "child_exit_code": process.returncode,
            "child_workload_report": options.workload_report.name,
            "child_compiled_inputs_sha256": None if child is None else child.get("compiled_inputs_sha256"),
            "os": sys.platform, "wall_ns": wall_ns, "cpu_user_time_ns": user_ns,
            "cpu_kernel_time_ns": kernel_ns, "cpu_time_ns": cpu_ns, "cpu_measurement": cpu_method,
            "average_cpu_percent_of_one_core": None if cpu_ns is None else cpu_ns / wall_ns * 100,
            "memory_sampling": samples.report(wall_ns, round(options.sample_ms * 1_000_000)),
            "resource_query_failures": failures, "last_resource_query_error": last_error,
            "timed_out": timed_out, "verification_error": verification_error,
            "limitations": [
                "Measures only the spawned synthetic workload, not the existing client or sampler",
                "Sampled RSS can miss spikes; start/tail and sample gaps are reported",
                "Observed OS high-water mark is through the last successful query, not a heap allocation profile",
                "Average CPU percent uses one logical core as 100%; ambient machine load remains uncontrolled",
                "No sustained swarm, cold-start, production throughput or leak-absence claim",
            ],
        }
        json.dump(report, report_file, indent=2)
        report_file.write("\n")
    return 0 if completed else 1


def tempfile_stderr():
    import tempfile

    return tempfile.TemporaryFile()


if __name__ == "__main__":
    raise SystemExit(main())
