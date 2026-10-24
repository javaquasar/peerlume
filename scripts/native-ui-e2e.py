"""Real X11 window + mouse input + copied CLI + loopback transfer and repair.

Run only on an isolated CI desktop. No application test mode or permission bypass.
Tokens are obtained through the trusted GUI clipboard action and never persisted.
"""
import argparse
import csv
import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import re
import shutil
import socket
import socketserver
import sqlite3
import struct
import subprocess
import tempfile
import threading
import time


def wait_for(action, timeout=60):
    end = time.monotonic() + timeout
    last = None
    while time.monotonic() < end:
        last = action()
        if last:
            return last
        time.sleep(0.2)
    raise AssertionError("Native workflow timed out")


def bencode(value):
    if isinstance(value, int):
        return b"i" + str(value).encode() + b"e"
    if isinstance(value, bytes):
        return str(len(value)).encode() + b":" + value
    if isinstance(value, dict):
        return b"d" + b"".join(bencode(key) + bencode(value[key]) for key in sorted(value)) + b"e"
    raise TypeError("Unsupported fixture value")


def exact(stream, count):
    result = bytearray()
    while len(result) < count:
        chunk = stream.recv(count - len(result))
        if not chunk:
            raise EOFError()
        result.extend(chunk)
    return bytes(result)


class Fixtures:
    def __init__(self):
        self.payload = bytes(index % 251 for index in range(512 * 1024))
        self.piece_length = 16384
        self.requests = {}
        self.lock = threading.Lock()
        owner = self

        class Peer(socketserver.BaseRequestHandler):
            def handle(self):
                try:
                    self.request.settimeout(30)
                    handshake = exact(self.request, 68)
                    assert handshake[28:48] == owner.info_hash
                    self.request.sendall(handshake[:48] + b"-PLTEST-native000001")
                    bitfield = b"\xff" * (len(owner.payload) // owner.piece_length // 8)
                    self.request.sendall(struct.pack(">I", 1 + len(bitfield)) + b"\x05" + bitfield)
                    self.request.sendall(b"\0\0\0\1\x01")
                    while True:
                        length = struct.unpack(">I", exact(self.request, 4))[0]
                        assert length <= 65536
                        message = exact(self.request, length)
                        if length != 13 or message[0] != 6:
                            continue
                        index, begin, count = struct.unpack(">III", message[1:])
                        assert 0 < count <= 16384
                        start = index * owner.piece_length + begin
                        block = owner.payload[start:start + count]
                        assert len(block) == count
                        with owner.lock:
                            owner.requests[index] = owner.requests.get(index, 0) + 1
                        time.sleep(0.005)
                        body = b"\x07" + struct.pack(">II", index, begin) + block
                        self.request.sendall(struct.pack(">I", len(body)) + body)
                except (EOFError, OSError):
                    return

        class Server(socketserver.ThreadingTCPServer):
            daemon_threads = True

        self.peer = Server(("127.0.0.1", 0), Peer)

        class Tracker(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                peer = socket.inet_aton("127.0.0.1") + struct.pack(">H", owner.peer.server_address[1])
                body = bencode({b"interval": 1, b"peers": peer})
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self.tracker = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Tracker)
        info = {b"name": b"payload.bin", b"length": len(self.payload), b"piece length": self.piece_length,
                b"pieces": b"".join(hashlib.sha1(self.payload[i:i + self.piece_length]).digest()
                                      for i in range(0, len(self.payload), self.piece_length)), b"private": 1}
        self.info_hash = hashlib.sha1(bencode(info)).digest()
        self.metainfo = bencode({b"announce": f"http://127.0.0.1:{self.tracker.server_address[1]}/announce".encode(), b"info": info})
        for server in (self.peer, self.tracker):
            threading.Thread(target=server.serve_forever, daemon=True).start()

    def close(self):
        for server in (self.peer, self.tracker):
            server.shutdown()
            server.server_close()


class Desktop:
    def __init__(self, report):
        self.report = report
        self.window = None

    def screenshot(self, name):
        path = self.report / f"{name}.png"
        subprocess.run(["import", "-window", "root", str(path)], check=True, timeout=10)
        return path

    def words(self):
        image = self.screenshot("current")
        scaled = self.report / "ocr.png"
        subprocess.run(["convert", str(image), "-resize", "200%", str(scaled)], check=True, timeout=10)
        result = subprocess.run(["tesseract", str(scaled), "stdout", "--psm", "11", "tsv"], capture_output=True, check=True, timeout=15)
        return [row for row in csv.DictReader(io.StringIO(result.stdout.decode()), delimiter="\t") if row.get("text", "").strip()]

    @staticmethod
    def normalize(text):
        return re.sub("[^a-z0-9]", "", text.lower())

    def locate(self, phrase, last=False, minimum_y=0):
        words = self.words()
        needle = [self.normalize(word) for word in phrase.split()]
        matches = []
        for index in range(len(words) - len(needle) + 1):
            block = words[index:index + len(needle)]
            if [self.normalize(row["text"]) for row in block] != needle:
                continue
            x = (int(block[0]["left"]) + int(block[-1]["left"]) + int(block[-1]["width"])) // 4
            y = (int(block[0]["top"]) + int(block[0]["height"]) // 2) // 2
            if y >= minimum_y:
                matches.append((x, y))
        return matches[-1 if last else 0] if matches else None

    def click(self, phrase, last=False, minimum_y=0):
        position = wait_for(lambda: self.locate(phrase, last, minimum_y), timeout=25)
        subprocess.run(["xdotool", "mousemove", "--sync", str(position[0]), str(position[1]), "click", "1"], check=True)
        time.sleep(0.5)

    def attach(self, app):
        def window():
            assert app.poll() is None, "GUI exited before creating its native window; see application.log"
            result = subprocess.run(["xdotool", "search", "--onlyvisible", "--name", "^PeerLume$"], capture_output=True)
            return result.stdout.decode().splitlines() if result.returncode == 0 else None
        self.window = wait_for(window, timeout=45)[-1]
        subprocess.run(["xdotool", "windowactivate", "--sync", self.window], check=True)

    def add(self):
        wait_for(lambda: self.locate("Review torrent before adding"), timeout=30)
        for _ in range(15):
            if self.locate("Start after adding"):
                break
            subprocess.run(["xdotool", "mousemove", "600", "500", "click", "--repeat", "5", "5"], check=True)
            time.sleep(0.2)
        self.screenshot("01-add-preview")
        self.click("Add torrent", last=True, minimum_y=200)

    def grant(self, new=True):
        self.click("Agent access")
        self.click("Enable local IPC")
        token = None
        if new:
            self.click("Manage downloads")
            self.click("Create token with these grants")
            self.click("Copy token")
            token = subprocess.run(["xclip", "-selection", "clipboard", "-o"], capture_output=True, check=True, timeout=5).stdout.decode().strip()
            assert re.fullmatch("[0-9a-f]{64}", token), "GUI did not copy a valid token"
            subprocess.run(["xclip", "-selection", "clipboard"], input=b"", check=True, timeout=5)
            self.click("Forget displayed token")
        self.screenshot("02-agent-approved")
        self.click("Agent access")
        return token

    def close(self):
        subprocess.run(["xdotool", "windowclose", self.window], check=True)


def run(args):
    report = Path(args.report).resolve()
    report.mkdir(parents=True, exist_ok=True)
    desktop = Desktop(report)
    fixture = Fixtures()
    app = None
    try:
        with tempfile.TemporaryDirectory(prefix="peerlume-native-") as temporary:
            root = Path(temporary)
            profile = root / "profile"
            profile.mkdir()
            downloads = root / "downloads"
            downloads.mkdir()
            installed = root / "installed"
            installed.mkdir()
            gui = Path(shutil.copy2(args.gui, installed / "peerlume"))
            cli = Path(shutil.copy2(args.cli, installed / "peerlume-control"))
            source = root / "fixture.torrent"
            source.write_bytes(fixture.metainfo)
            (profile / "settings.toml").write_text('version = 2\n[download]\ndownload_directory = ' + json.dumps(str(downloads)) + '\n[bittorrent]\ndht = false\npex = false\nlsd = false\n', encoding="utf-8")
            log = (report / "application.log").open("wb")

            def launch(first):
                command = [str(gui), "--profile", str(profile)]
                if first:
                    command += ["--open", str(source)]
                return subprocess.Popen(command, cwd=installed, stdout=log, stderr=subprocess.STDOUT)

            app = launch(True)
            desktop.attach(app)
            desktop.add()
            target = downloads / "payload.bin"
            expected = hashlib.sha256(fixture.payload).digest()

            def verified_file():
                return target.exists() and hashlib.sha256(target.read_bytes()).digest() == expected

            wait_for(verified_file, timeout=120)
            token = desktop.grant()
            instance = None

            def request(name, arguments=None, explicit_instance=None, allow_error=False):
                environment = os.environ.copy()
                environment["PEERLUME_CONTROL_TOKEN"] = token
                message = {"instance_id": instance if explicit_instance is None else explicit_instance, "method": {"name": name}}
                if arguments is not None:
                    message["method"]["args"] = arguments
                reply = subprocess.run([str(cli), "request", "--profile", str(profile)], input=json.dumps(message).encode() + b"\n", capture_output=True, env=environment, timeout=15)
                response = json.loads(reply.stdout)
                assert not reply.stderr, "CLI emitted unexpected stderr"
                assert allow_error or response["error"] is None, f"CLI request failed: {response['error']}"
                return response

            instance = request("discover")["instance_id"]

            def row():
                return request("list", {"offset": 0, "limit": 1, "revision": None, "after_revision": None})["result"]["torrents"][0]

            wait_for(lambda: row()["completion"] == "verified")
            desktop.click("Pause all")
            wait_for(lambda: row()["state"] == "paused")
            desktop.screenshot("03-native-pause")
            desktop.click("Resume all")
            wait_for(lambda: row()["state"] == "seeding")

            def mutate(command, allow_failed=False):
                current = row()
                operation = request("mutate", {"torrent_id": current["id"], "command": command, "expected_revision": current["command_revision"], "idempotency_key": f"native-{time.monotonic_ns()}", "deadline_unix_ms": int(time.time() * 1000) + 60000})["result"]["operation_id"]

                def terminal():
                    result = request("operation", {"operation_id": operation})["result"]
                    return result if result["state"] in ("succeeded", "failed", "cancelled", "reconciliation_required") else None

                result = wait_for(terminal)
                assert result["state"] == "succeeded" or (allow_failed and result["state"] == "failed"), "Unexpected command result"

            mutate({"kind": "set_repair_policy", "value": {"enabled": True, "max_attempts": 3}})
            time.sleep(2)
            wait_for(lambda: row()["completion"] == "verified" and row()["state"] == "seeding")
            with fixture.lock:
                before = fixture.requests.get(0, 0)
            with target.open("r+b") as output:
                output.write(b"\xff")
                output.flush()
                os.fsync(output.fileno())
            assert not verified_file()
            mutate({"kind": "recheck"}, allow_failed=True)
            wait_for(verified_file, timeout=120)
            wait_for(lambda: row()["completion"] == "verified")
            with fixture.lock:
                after = fixture.requests.get(0, 0)
            assert after > before, "Damaged piece was not fetched again from the real peer"
            assert row()["active_mode"] == "normal"
            wait_for(lambda: desktop.locate("Done"), timeout=30)
            desktop.screenshot("04-repaired-done")
            desktop.click("Pause all")
            wait_for(lambda: row()["state"] == "paused")
            with sqlite3.connect(profile / "state.sqlite3") as database:
                assert database.execute("SELECT state FROM torrents").fetchone()[0] == "paused"
            old_instance = instance
            desktop.close()
            app.wait(timeout=20)
            app = launch(False)
            desktop.attach(app)
            desktop.grant(new=False)
            assert request("list", {"offset": 0, "limit": 1, "revision": None, "after_revision": None}, explicit_instance=old_instance, allow_error=True)["error"] == "conflict"
            instance = None
            instance = request("discover")["instance_id"]
            assert instance != old_instance
            wait_for(lambda: row()["state"] == "paused" and row()["completion"] == "verified")
            assert verified_file()
            desktop.screenshot("05-restored-paused")
            (report / "result.json").write_text(json.dumps({"native_window": True, "gui_mouse_add_pause_resume": True, "external_cli": True, "fresh_peer_download": True, "damaged_piece_refetched": after - before, "restart_preserved_pause": True, "reporting_mode": "normal"}, indent=2), encoding="utf-8")
            desktop.close()
            app.wait(timeout=20)
            app = None
            log.close()
    finally:
        if app is not None and app.poll() is None:
            desktop.screenshot("failure")
            app.terminate()
            try:
                app.wait(timeout=10)
            except subprocess.TimeoutExpired:
                app.kill()
                app.wait(timeout=5)
        fixture.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--gui", required=True)
    parser.add_argument("--cli", required=True)
    parser.add_argument("--report", required=True)
    run(parser.parse_args())
