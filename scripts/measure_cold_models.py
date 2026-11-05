#!/usr/bin/env python3
"""Hold loaded/cold/summary synthetic models while sampling the child's RSS."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import time

from sample_perf_process import LinuxProbe, SampleSummary, WindowsProbe


def validate_phase(message, phase, files, identity):
    if message.get("workload") != "cold_model_phase_0_3_0" or message.get("phase") != phase or message.get("identity") != identity:
        raise ValueError("Cold-model phase identity differs")
    if message.get("files") != files or message.get("selected_count") != files or message.get("total_size") != files * 262_144 or message.get("selected_size") != files * 262_144 or message.get("selected_downloaded") != 0:
        raise ValueError("Cold-model authoritative totals differ")
    buffers=message["accounted_buffers"]
    names=("file_slots_bytes","path_capacity_bytes","tree_capacity_bytes","view_capacity_bytes","compact_capacity_bytes","priority_capacity_bytes")
    if any(not isinstance(buffers[name],int) or buffers[name]<0 for name in names) or sum(buffers[name] for name in names)!=buffers["total_bytes"]:
        raise ValueError("Accounted model buffer totals differ")
    if phase=="loaded" and (message.get("details_loaded") is not True or buffers["path_capacity_bytes"]==0):
        raise ValueError("Loaded phase has no genuine path detail")
    if phase!="loaded" and (message.get("details_loaded") is not False or any(buffers[name] for name in ("file_slots_bytes","path_capacity_bytes","tree_capacity_bytes","view_capacity_bytes"))):
        raise ValueError("Cold phase still retains loaded file detail")
    if phase=="owner_cold" and buffers["compact_capacity_bytes"]==0:
        raise ValueError("Owner cold phase lost its compact per-file state")
    if phase=="summary_mirror" and buffers["total_bytes"]!=0:
        raise ValueError("Summary mirror still retains per-file buffers")
    if not isinstance(message.get("inline_model_bytes"),int) or message["inline_model_bytes"]<=0:
        raise ValueError("Inline model bytes are unavailable")


def reader(stream, messages):
    try:
        while True:
            line=stream.readline(64*1024+1)
            if not line:
                return
            if len(line)>64*1024:
                raise ValueError("Child phase frame exceeds its bound")
            messages.put(json.loads(line),timeout=10)
    except Exception as error:
        messages.put({"reader_error":type(error).__name__},timeout=10)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--executable",type=Path,required=True)
    parser.add_argument("--report",type=Path,required=True)
    parser.add_argument("--source-label",required=True)
    parser.add_argument("--samples",type=int,default=20)
    parser.add_argument("--sample-ms",type=float,default=10)
    options=parser.parse_args()
    if not 1<=options.samples<=100 or not 1<=options.sample_ms<=100:
        parser.error("samples must be 1..100 and sample-ms 1..100")
    if options.report.exists():
        parser.error("report path must be new")
    executable=options.executable.resolve(strict=True)
    flags=getattr(subprocess,"CREATE_NO_WINDOW",0)
    identity=json.loads(subprocess.check_output([str(executable),"--describe"],creationflags=flags,timeout=10))
    cases=[]
    for files in (1000,10_000,100_000):
        case={"files":files,"phases":[],"completed":False}
        started=time.perf_counter_ns()
        with tempfile.TemporaryFile() as errors:
            with subprocess.Popen([str(executable),"--operation","memory","--files",str(files)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=errors,creationflags=flags) as process:
                probe=None
                messages=queue.Queue(maxsize=4)
                worker=threading.Thread(target=reader,args=(process.stdout,messages),daemon=True)
                worker.start()
                try:
                    probe=WindowsProbe(process) if os.name=="nt" else LinuxProbe(process)
                    for phase in ("loaded","owner_cold","summary_mirror"):
                        message=messages.get(timeout=10)
                        validate_phase(message,phase,files,identity)
                        summary=SampleSummary()
                        phase_started=time.perf_counter_ns()
                        for _ in range(options.samples):
                            rss,high=probe.memory()
                            summary.observe(time.perf_counter_ns()-phase_started,rss,high)
                            time.sleep(options.sample_ms/1000)
                        phase_wall=time.perf_counter_ns()-phase_started
                        message["memory_sampling"]=summary.report(phase_wall,round(options.sample_ms*1_000_000))
                        if summary.count!=options.samples:
                            raise ValueError("Phase RSS samples are incomplete")
                        case["phases"].append(message)
                        process.stdin.write(b"continue\n")
                        process.stdin.flush()
                    final=messages.get(timeout=10)
                    if final.get("completed") is not True or final.get("operation")!="memory" or final.get("identity")!=identity:
                        raise ValueError("Child did not complete the phased workload")
                    process.wait(timeout=10)
                    user,kernel,method=probe.final_cpu()
                    case.update({"completed":process.returncode==0,"child_exit_code":process.returncode,"cpu_user_time_ns":user,"cpu_kernel_time_ns":kernel,"cpu_time_ns":user+kernel,"cpu_measurement":method})
                except Exception as error:
                    case["error_type"]=type(error).__name__
                    if isinstance(error,ValueError):
                        case["error"]=str(error)
                finally:
                    if process.poll() is None:
                        process.kill()  # Only this sampler's disposable fixture.
                        process.wait()
                    if probe is not None:
                        probe.close()
                    worker.join(timeout=1)
        case["process_wall_ns"]=time.perf_counter_ns()-started
        cases.append(case)
    report={"schema_version":1,"workload":"cold_model_resources_0_3_0","source_label":options.source_label,"executable_file_name":executable.name,"executable_sha256":hashlib.sha256(executable.read_bytes()).hexdigest(),"identity":identity,"completed":all(case["completed"] for case in cases),"samples_per_phase":options.samples,"requested_sample_interval_ms":options.sample_ms,"cases":cases,"limitations":["Synthetic file model only; excludes engine metadata, SQLite and native frontend allocations","Accounted buffers exclude inline object bytes, fixed Arc/Mutex allocations and allocator overhead","Each phase is held until the sampler acknowledges it; wall time includes deliberate idle holds","Per-phase sampled RSS is not lifetime RSS peak or private heap; allocator retention can keep RSS high after buffers are freed","OS high-water marks include earlier phases; do not label them as cold-phase-only peaks","No user profile, tracker, payload files or running client is sampled"]}
    with options.report.open("x",encoding="utf-8") as output:
        json.dump(report,output,indent=2)
        output.write("\n")
    return 0 if report["completed"] else 1


if __name__=="__main__":
    raise SystemExit(main())
