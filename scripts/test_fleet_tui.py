#!/usr/bin/env python3
"""Offline PTY proof: real TUI lifecycle, preview/cancel and confirmed rotation.

Run after `cargo build --locked`: python3 scripts/test_fleet_tui.py
Uses only Python stdlib, isolated HOME/config, synthetic secrets and loopback HTTP.
"""
import base64
import re
import fcntl
import http.server
import json
import os
import pathlib
import pty
import select
import struct
import subprocess
import tempfile
import termios
import threading
import time
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        if self.path != "/admin/providers/sources":
            self.send_error(404)
            return
        body = json.dumps([{"name": "fixture", "provider_type": "openai", "source": "admin",
                            "revision": "2026-10-07T00:00:00Z", "enabled": True}]).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        payload = json.loads(self.rfile.read(length))
        self.server.writes.append((self.path, payload))
        body = (b'{"code":"synthetic-provider-secret","error":"private"}' if self.server.reject_mutations
                else b'{"name":"fixture","revision":"2026-10-07T00:00:01Z"}')
        self.send_response(409 if self.server.reject_mutations else 200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class FleetTuiTest(unittest.TestCase):
    def assert_private_status(self, captured):
        for encoded in re.findall(rb":(?:msg|title)=([A-Za-z0-9+/=]*)", captured):
            decoded = base64.b64decode(encoded)
            self.assertNotIn(b"synthetic-admin", decoded)
            self.assertNotIn(b"synthetic-provider-secret", decoded)

    def test_preview_cancel_then_confirm_and_restore_terminal(self):
        self.run_scenario()

    def test_disabled_status_does_not_emit(self):
        self.run_scenario(disabled=True)

    def test_failed_apply_reports_error_without_retry(self):
        self.run_scenario(failure=True)

    def test_json_inventory_on_terminal_never_emits(self):
        self.run_cli_on_terminal(json_mode=True)

    def test_cli_apply_reports_working_and_done(self):
        self.run_cli_on_terminal()

    def test_cli_failure_reports_error(self):
        self.run_cli_on_terminal(failure=True)

    def run_cli_on_terminal(self, json_mode=False, failure=False):
        executable = pathlib.Path(os.environ.get("EAVS_TEST_BINARY", ROOT / "target/debug/eavs"))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        server.writes = []
        server.reject_mutations = failure
        threading.Thread(target=server.serve_forever, daemon=True).start()
        master, slave = pty.openpty()
        try:
            with tempfile.TemporaryDirectory(prefix="eavs-status-json-") as home:
                config = pathlib.Path(home) / "fleet.toml"
                config.write_text(f'[[instances]]\nname = "fixture-host"\nurl = "http://127.0.0.1:{server.server_port}"\nadmin_key_env = "FIXTURE_ADMIN"\n')
                arguments = [str(executable), "fleet", "--fleet", str(config)]
                arguments += (["list", "--json"] if json_mode else
                              ["rotate", "fixture", "--instance", "fixture-host", "--key-env", "FIXTURE_PROVIDER", "--yes"])
                process = subprocess.Popen(arguments, stdin=slave, stdout=slave, stderr=slave,
                                           env={"PATH": os.environ.get("PATH", ""), "HOME": home, "FIXTURE_ADMIN": "synthetic-admin",
                                                "FIXTURE_PROVIDER": "synthetic-provider-secret"})
                try:
                    self.assertEqual(process.wait(timeout=5), 1 if failure else 0)
                    captured = bytearray()
                    while select.select([master], [], [], 0.1)[0]:
                        captured.extend(os.read(master, 65536))
                    self.assertNotIn(b"synthetic-admin", captured)
                    self.assertNotIn(b"synthetic-provider-secret", captured)
                    self.assert_private_status(captured)
                    if json_mode:
                        self.assertNotIn(b"\x1b]7501;", captured)
                        self.assertEqual(json.loads(captured)["instances"][0]["name"], "fixture-host")
                        self.assertEqual(server.writes, [])
                    else:
                        self.assertIn(b"\x1b]7501;state=working:", captured)
                        self.assertIn(b"\x1b]7501;state=" + (b"error:" if failure else b"done:"), captured)
                        self.assertEqual(len(server.writes), 1)
                    checker = os.environ.get("BYTEOWLZ_OSC7501_CHECKER")
                    if checker:
                        import sys
                        flags = ["--expect-none"] if json_mode else ["--require-state", "working", "--require-state", "error" if failure else "done"]
                        checked = subprocess.run([sys.executable, checker, "--json"] + flags, input=bytes(captured), capture_output=True, check=False)
                        self.assertEqual(checked.returncode, 0, checked.stdout + checked.stderr)
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.wait(timeout=5)
        finally:
            os.close(master)
            os.close(slave)
            server.shutdown()
            server.server_close()

    def test_redirected_cli_mutation_never_emits(self):
        executable = pathlib.Path(os.environ.get("EAVS_TEST_BINARY", ROOT / "target/debug/eavs"))
        with tempfile.TemporaryDirectory(prefix="eavs-status-no-tty-") as home:
            run = subprocess.run([str(executable), "fleet", "--fleet", str(pathlib.Path(home) / "absent.toml"),
                                  "rotate", "fixture", "--instance", "fixture-host", "--key-env", "UNREAD_KEY", "--yes"],
                                 capture_output=True, env={"PATH": os.environ.get("PATH", ""), "HOME": home,
                                                           "EAVS_PROGRAM_STATUS": "1"}, check=False)
            self.assertNotEqual(run.returncode, 0)
            self.assertNotIn(b"\x1b]7501;", run.stdout + run.stderr)

    def run_scenario(self, disabled=False, failure=False):
        executable = pathlib.Path(os.environ.get("EAVS_TEST_BINARY", ROOT / "target/debug/eavs"))
        self.assertTrue(executable.is_file(), "Run cargo build --locked first")
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        server.writes = []
        server.reject_mutations = failure
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        with tempfile.TemporaryDirectory(prefix="eavs-tui-proof-") as home:
            config = pathlib.Path(home) / "fleet.toml"
            config.write_text(f'[[instances]]\nname = "fixture-host"\nurl = "http://127.0.0.1:{server.server_port}"\nadmin_key_env = "FIXTURE_ADMIN"\n')
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 26, 110, 0, 0))
            original = termios.tcgetattr(slave)
            environment = {"PATH": os.environ.get("PATH", ""), "HOME": home, "TERM": "xterm-256color",
                           "FIXTURE_ADMIN": "synthetic-admin", "FIXTURE_PROVIDER": "synthetic-provider-secret"}
            if disabled:
                environment["EAVS_PROGRAM_STATUS"] = "0"
            process = subprocess.Popen([str(executable), "tui", "--fleet", str(config)],
                                       stdin=slave, stdout=slave, stderr=slave, env=environment)
            captured = bytearray()

            def drain(seconds=0.3):
                deadline = time.monotonic() + seconds
                while time.monotonic() < deadline:
                    if select.select([master], [], [], 0.05)[0]:
                        captured.extend(os.read(master, 65536))

            try:
                drain(1.5)
                self.assertIn(b"Ready", captured)
                self.assertIn(b"fixture-host", captured)
                self.assertEqual(server.writes, [])
                os.write(master, b"c\tFIXTURE_PROVIDER\r")
                drain()
                # Ratatui emits cell diffs, so a title may be split by cursor moves.
                self.assertIn(b"ROTATE fixture", captured)
                self.assertEqual(server.writes, [], "Preview must not send a mutation")
                os.write(master, b"\x1b")
                drain()
                self.assertEqual(server.writes, [], "Escape must discard the plan")
                os.write(master, b"c\tFIXTURE_PROVIDER\r")
                drain()
                os.write(master, b"\r")
                drain(1.2)
                self.assertEqual(len(server.writes), 1)
                path, payload = server.writes[0]
                self.assertEqual(path, "/admin/providers/fixture/credentials")
                self.assertEqual(payload["expected_revision"], "2026-10-07T00:00:00Z")
                self.assertEqual(payload["api_key"], "synthetic-provider-secret")
                self.assertNotIn(b"synthetic-provider-secret", captured)
                self.assertNotIn(b"synthetic-admin", captured)
                os.write(master, b"q")
                drain()
                self.assertEqual(process.wait(timeout=5), 0)
                self.assertEqual(termios.tcgetattr(slave), original, "RAII must restore terminal flags")
                self.assert_private_status(captured)
                if disabled:
                    self.assertNotIn(b"\x1b]7501;", captured)
                else:
                    for state in (b"working", b"idle", b"blocked", b"error" if failure else b"done"):
                        self.assertIn(b"\x1b]7501;state=" + state, captured)
                    self.assertIn(b"state=blocked:kind=question", captured)
                    self.assertIn(b"state=blocked:kind=permission", captured)
                checker = os.environ.get("BYTEOWLZ_OSC7501_CHECKER")
                if checker:
                    import sys
                    arguments = [sys.executable, checker, "--json"]
                    if disabled:
                        arguments += ["--expect-none"]
                    else:
                        for state in ("working", "idle", "blocked", "error" if failure else "done"):
                            arguments += ["--require-state", state]
                    checked = subprocess.run(arguments, input=bytes(captured), capture_output=True, check=False)
                    self.assertEqual(checked.returncode, 0, checked.stdout + checked.stderr)
                    self.assertTrue(json.loads(checked.stdout)["valid"])
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=5)
                os.close(master)
                os.close(slave)
                server.shutdown()
                server.server_close()


if __name__ == "__main__":
    unittest.main()
