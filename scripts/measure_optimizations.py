#!/usr/bin/env python3
"""Bounded synthetic path/import and journal workload runner; no user profile."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--executable",type=Path,required=True)
    parser.add_argument("--report",type=Path,required=True)
    parser.add_argument("--source-label",required=True)
    parser.add_argument("--timeout-seconds",type=float,default=15)
    options=parser.parse_args()
    if not 1<=options.timeout_seconds<=120:
        parser.error("timeout must be 1..120 seconds per child")
    if options.report.exists():
        parser.error("report path must be new")
    executable=options.executable.resolve(strict=True)
    flags=getattr(subprocess,"CREATE_NO_WINDOW",0)
    identity=json.loads(subprocess.check_output([str(executable),"--describe"],creationflags=flags,timeout=10))
    results=[]
    for operation,files in [(op,count) for op in ("validate","import") for count in (1000,10_000,100_000)]+[("journal",1000)]:
        started=time.perf_counter_ns()
        try:
            child=subprocess.run([str(executable),"--operation",operation,"--files",str(files)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,creationflags=flags,timeout=options.timeout_seconds,check=False)
            result=json.loads(child.stdout) if child.returncode==0 else {"completed":False,"child_exit_code":child.returncode,"error":child.stderr[-4096:].decode("utf-8",errors="replace")}
            if result.get("completed") is True and (result.get("operation")!=operation or result.get("files")!=files or result.get("identity")!=identity):
                raise ValueError("Child identity or workload inputs differ")
        except subprocess.TimeoutExpired:
            result={"completed":False,"timed_out":True,"timeout_seconds":options.timeout_seconds}
        result.update({"operation":operation,"files":files,"process_wall_ns":time.perf_counter_ns()-started})
        results.append(result)
    report={"schema_version":1,"workload":"optimization_comparison_0_3_0","source_label":options.source_label,"executable_file_name":executable.name,"executable_sha256":hashlib.sha256(executable.read_bytes()).hexdigest(),"identity":identity,"cases":results,"completed":all(case.get("completed") is True for case in results),"limitations":["Wall-clock synthetic workloads; no network or user profile","Each child is bounded; timed-out cases retain their exact timeout instead of a guessed duration","No real-client RSS, sustained throughput or leak-absence claim"]}
    with options.report.open("x",encoding="utf-8") as output:
        json.dump(report,output,indent=2)
        output.write("\n")
    return 0 if report["completed"] else 1


if __name__=="__main__":
    raise SystemExit(main())
