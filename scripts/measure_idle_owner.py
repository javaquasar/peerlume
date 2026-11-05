#!/usr/bin/env python3
"""Sample one disposable empty daemon, never a user profile or existing PID."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
import time

from sample_perf_process import LinuxProbe, SampleSummary, WindowsProbe


def fixture_settings(profile):
    root = json.dumps(str(profile / "downloads"), ensure_ascii=False)
    return f"""version = 2
[download]
download_directory = {root}
[connection]
random_port_on_start = true
port_forwarding = false
utp_enabled = false
[bittorrent]
dht = false
pex = false
lsd = false
public_extra_trackers = ""
[privacy]
blocklist_enabled = false
blocklist_auto_update = false
"""


def proc_cpu_times(contents, ticks):
    fields = contents.rsplit(")", 1)[1].split()
    if len(fields) < 13 or ticks <= 0:
        raise ValueError("Incomplete process CPU counters")
    # state is field 3; utime/stime are fields 14/15 in proc_pid_stat(5).
    return (int(fields[11]) * 1_000_000_000 // ticks,
            int(fields[12]) * 1_000_000_000 // ticks)


def live_cpu(probe, process):
    if os.name == "nt":
        user, kernel, _ = probe.final_cpu()
        return user, kernel
    return proc_cpu_times(Path(f"/proc/{process.pid}/stat").read_text(), os.sysconf("SC_CLK_TCK"))


def bounded_probe(executable, profile):
    with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors:
        started = time.perf_counter_ns()
        process = subprocess.Popen([str(executable), "--profile", str(profile), "--attach-check"],
                                   stdin=subprocess.DEVNULL, stdout=output, stderr=errors,
                                   creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        process.wait(timeout=20)
        elapsed = time.perf_counter_ns() - started
        errors.seek(0)
        if process.returncode:
            raise RuntimeError("Native attach probe failed: " + errors.read(4096).decode(errors="replace"))
        output.seek(0)
        reply = json.loads(output.read(65_537))
        return elapsed, reply


def run(options):
    daemon = options.daemon.resolve(strict=True)
    frontend = options.frontend.resolve(strict=True)
    if daemon.name == frontend.name:
        raise ValueError("Daemon and frontend must have distinct package filenames")
    report = {"schema_version": 1, "workload": "empty_owner_idle_0_3_0", "completed": False,
              "source_label": options.source_label, "os": platform.system(), "architecture": platform.machine(),
              "daemon_sha256": hashlib.sha256(daemon.read_bytes()).hexdigest(),
              "frontend_sha256": hashlib.sha256(frontend.read_bytes()).hexdigest(),
              "fixture": {"torrents": 0, "dht": False, "pex": False, "lsd": False, "upnp": False,
                          "public_extra_trackers": False, "utp": False, "blocklist_update": False,
                          "listener": "engine default; fixture is not OS network isolation"},
              "limitations": ["Optimized only: no paired baseline or CPU-reduction percentage",
                              "Empty disposable owner; not an active swarm or a long soak",
                              "Attach latency includes frontend process startup and native handshake/snapshot",
                              "Finite sampling can miss shorter RSS peaks; CPU timer granularity applies"]}
    with tempfile.TemporaryDirectory(prefix="peerlume-idle-0.3-", ignore_cleanup_errors=True) as temporary:
        root = Path(temporary)
        profile = root / "isolated profile"
        (profile / "downloads").mkdir(parents=True)
        (profile / "settings.toml").write_text(fixture_settings(profile), encoding="utf-8")
        copied_daemon = root / daemon.name
        copied_frontend = root / frontend.name
        shutil.copy2(daemon, copied_daemon)
        shutil.copy2(frontend, copied_frontend)
        probe = None
        with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors:
            owner = subprocess.Popen([str(copied_daemon), "--profile", str(profile)], stdin=subprocess.DEVNULL,
                                     stdout=output, stderr=errors, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
            try:
                probe = WindowsProbe(owner) if os.name == "nt" else LinuxProbe(owner)
                deadline = time.monotonic() + 15
                descriptor = profile / "frontend-auth.json"
                while not descriptor.is_file():
                    if owner.poll() is not None:
                        errors.seek(0)
                        raise RuntimeError("Disposable owner exited during startup: " + errors.read(4096).decode(errors="replace"))
                    if time.monotonic() >= deadline:
                        raise TimeoutError("Disposable owner readiness exceeded15 seconds")
                    time.sleep(0.02)
                owner_id = json.loads(descriptor.read_text())["instance_id"]
                time.sleep(1)
                before_user, before_kernel = live_cpu(probe, owner)
                summary = SampleSummary()
                started = time.perf_counter_ns()
                while (elapsed := time.perf_counter_ns() - started) < 2_000_000_000:
                    if owner.poll() is not None:
                        raise RuntimeError("Disposable owner exited during idle sampling")
                    rss, high = probe.memory()
                    summary.observe(elapsed, rss, high)
                    time.sleep(min(0.1, max(0, (2_000_000_000 - elapsed) / 1_000_000_000)))
                wall = time.perf_counter_ns() - started
                after_user, after_kernel = live_cpu(probe, owner)
                report["idle"] = {"wall_ns": wall, "cpu_user_time_ns": after_user - before_user,
                                  "cpu_kernel_time_ns": after_kernel - before_kernel,
                                  "cpu_time_ns": after_user + after_kernel - before_user - before_kernel,
                                  "cpu_method": "GetProcessTimes live-owner delta" if os.name == "nt" else "proc_pid_stat live-owner delta",
                                  "memory_sampling": summary.report(wall, 100_000_000)}
                if owner.poll() is not None:
                    raise RuntimeError("Owner exited before native latency probe")
                elapsed, reply = bounded_probe(copied_frontend, profile)
                if reply.get("owner_instance") != owner_id or reply.get("torrents") != 0:
                    raise ValueError("Native probe did not observe the same empty owner")
                report["attach_probe_process_wall_ns"] = elapsed
                report["owner_instance_unchanged"] = True
            except Exception as error:
                report["error"] = str(error).replace(str(root), "<disposable-profile>")
            finally:
                # An unexpected owner restart must remain a failed sample, but
                # still request normal cleanup of that disposable profile.
                if owner.poll() is None or (profile / "frontend-auth.json").is_file():
                    try:
                        with tempfile.TemporaryFile() as stop_output:
                            stop_started = time.perf_counter_ns()
                            stopped = subprocess.run([str(copied_daemon), "--profile", str(profile), "--stop"],
                                                     stdin=subprocess.DEVNULL, stdout=stop_output, stderr=stop_output,
                                                     creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0), timeout=100)
                            report["stop_cli_process_wall_ns"] = time.perf_counter_ns() - stop_started
                        report["stop_cli_exit_code"] = stopped.returncode
                        owner.wait(timeout=5)
                    except Exception as error:
                        report["shutdown_error"] = str(error).replace(str(root), "<disposable-profile>")
                report["owner_exit_code"] = owner.poll()
                if probe is not None:
                    probe.close()
            manifests = list((profile / "announce-logs").glob("*.manifest.json"))
            if len(manifests) == 1:
                manifest = json.loads(manifests[0].read_text())
                report["journal_shutdown"] = {"clean_shutdown": manifest.get("clean_shutdown"),
                                              "incomplete": manifest.get("incomplete"), "error": manifest.get("error"),
                                              "segments": len(manifest.get("segments", []))}
            report["completed"] = ("error" not in report and "shutdown_error" not in report
                                   and report.get("stop_cli_exit_code") == 0 and report.get("owner_exit_code") == 0
                                   and report.get("journal_shutdown") == {"clean_shutdown": True, "incomplete": False, "error": None, "segments": 1})
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--daemon", type=Path, required=True)
    parser.add_argument("--frontend", type=Path, required=True)
    parser.add_argument("--source-label", required=True)
    parser.add_argument("--report", type=Path, required=True)
    options = parser.parse_args()
    if options.report.exists():
        parser.error("report path must be new")
    report = run(options)
    with options.report.open("x", encoding="utf-8") as output:
        json.dump(report, output, indent=2)
        output.write("\n")
    return 0 if report["completed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
