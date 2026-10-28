"""Abrupt process death/restart on a fresh synthetic profile; disposable CI only."""
import argparse
import json
import os
from pathlib import Path
import queue
import re
import sqlite3
import subprocess
import tempfile
import threading
import time


def verify(binary, parent):
    if os.environ.get('GITHUB_ACTIONS') != 'true':
        raise RuntimeError('Process-kill smoke checks require a disposable CI runner')
    with tempfile.TemporaryDirectory(prefix='peerlume-crash-', dir=parent) as temporary:
        profile=Path(temporary)
        payload=profile / 'payload'
        payload.mkdir()
        (profile / 'settings.toml').write_text(
            'version = 2\n[download]\ndownload_directory = '+json.dumps(payload.as_posix())+
            '\n[connection]\nrandom_port_on_start = true\nport_forwarding = false\nutp_enabled = false\n'
            '[bittorrent]\ndht = false\nlsd = false\n[privacy]\ntracker_reporting = "disabled"\n',
            encoding='utf-8')
        sentinel=payload / 'keep.bin'
        sentinel.write_bytes(b'crash must preserve this payload')
        checked=subprocess.run([str(binary),'--profile',str(profile),'--check-config'],
                               capture_output=True,text=True,timeout=10)
        if checked.returncode:
            raise RuntimeError('Synthetic daemon configuration rejected: '+checked.stderr[-4096:])
        for attempt in range(2):
            flags=0
            startup=None
            if os.name=='nt':
                # The foreground daemon registers console signals. Give it its
                # own HIDDEN console, without opening a user-visible window.
                flags=subprocess.CREATE_NEW_CONSOLE
                startup=subprocess.STARTUPINFO()
                startup.dwFlags |= subprocess.STARTF_USESHOWWINDOW
                startup.wShowWindow=0
            error_path=profile / f'stderr-{attempt}.txt'
            with error_path.open('w',encoding='utf-8') as errors:
                def failure(reason):
                    errors.flush()
                    with error_path.open('rb') as diagnostics:
                        diagnostics.seek(0,2)
                        diagnostics.seek(max(0,diagnostics.tell()-4096))
                        detail=diagnostics.read(4096).decode('utf-8',errors='replace')
                    detail=re.sub(r'[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]','',detail)
                    return RuntimeError(f'{reason}; attempt {attempt+1}; stderr: '+detail.replace(str(profile),'<synthetic-profile>'))
                process=subprocess.Popen([str(binary),'--profile',str(profile)],stdout=subprocess.PIPE,
                                         stderr=errors,text=True,creationflags=flags,startupinfo=startup)
                lines=queue.Queue(maxsize=16)
                def read_ready():
                    for line in process.stdout:
                        try:
                            lines.put_nowait(line.strip())
                        except queue.Full:
                            pass
                reader=threading.Thread(target=read_ready,daemon=True)
                reader.start()
                try:
                    deadline=time.monotonic()+20
                    ready=False
                    while time.monotonic()<deadline and not ready:
                        if process.poll() is not None:
                            raise failure(f'Daemon exited before ready: {process.returncode}')
                        try:
                            ready=lines.get(timeout=0.1)=='PeerLume daemon ready'
                        except queue.Empty:
                            pass
                    if not ready:
                        raise failure('Daemon readiness timeout')
                    time.sleep(0.2)
                    if process.poll() is not None:
                        raise failure('Daemon exited immediately after readiness')
                    database=profile / 'state.sqlite3'
                    if not database.is_file():
                        raise RuntimeError('Ready daemon has no state database')
                    with sqlite3.connect(database) as connection:
                        if attempt==0:
                            connection.execute('CREATE TABLE installer_crash_probe(value TEXT PRIMARY KEY)')
                            connection.execute("INSERT INTO installer_crash_probe VALUES ('durable synthetic sentinel')")
                        else:
                            if connection.execute('PRAGMA quick_check').fetchone()!=('ok',):
                                raise RuntimeError('Database failed integrity check after abrupt death')
                            if connection.execute('SELECT value FROM installer_crash_probe').fetchall()!=[('durable synthetic sentinel',)]:
                                raise RuntimeError('Committed state lost after abrupt death')
                finally:
                    # Kill ONLY the exact process created above, never by name.
                    if process.poll() is None:
                        process.kill()
                    process.wait(timeout=10)
                    reader.join(timeout=2)
                    process.stdout.close()
            if (profile / 'fast-resume.json').exists():
                raise RuntimeError('Abrupt death left a false clean-resume marker')
            if sentinel.read_bytes()!=b'crash must preserve this payload':
                raise RuntimeError('Payload changed during empty-session crash recovery')
    print('Verified actual daemon abrupt death, same-profile restart, SQLite integrity and payload preservation')


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--directory',type=Path,required=True)
    args=parser.parse_args()
    verify(args.binary.resolve(strict=True),args.directory.resolve(strict=True))
