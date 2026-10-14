#!/usr/bin/env python3
"""Exercise the installed daemon boundary using loopback fixtures only.

Usage: python3 scripts/ci/event_watch_smoke.py --binary target/debug/zeroclaw
Build with channel-event-watch and gateway. No real accounts or external model calls.
"""
import argparse
import json
import os
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time
import urllib.request
import urllib.error
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--native", action="store_true")
    args = parser.parse_args()
    binary = args.binary.resolve()
    state = {"value": "on", "reads": 0, "model_calls": 0, "unexpected": []}

    class Fixture(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def reply(self, body, status=200):
            data = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            if self.path == "/v1/models":
                self.reply({"data": [{"id": "fixture"}]})
                return
            if self.path == "/api/states/light.example":
                state["reads"] += 1
                self.reply({"entity_id": "light.example", "state": state["value"]})
            else:
                state["unexpected"].append(self.path)
                self.reply({}, 404)

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            if self.path == "/v1/chat/completions":
                # Channel startup warms its provider. The event procedure itself
                # is deterministic and must not make additional model calls.
                state["model_calls"] += 1
                self.reply({"choices": [{"message": {"role": "assistant", "content": "fixture ready"}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 0, "completion_tokens": 0}})
                return
            method = request.get("method")
            if self.path != "/mcp":
                state["unexpected"].append(self.path)
                self.reply({}, 404)
                return
            if method == "notifications/initialized":
                self.send_response(204)
                self.end_headers()
                return
            if method == "initialize":
                result = {"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}, "serverInfo": {"name": "fixture", "version": "1"}}
            elif method == "tools/list":
                result = {"tools": []}
            else:
                state["unexpected"].append(str(method))
                result = {}
            self.reply({"jsonrpc": "2.0", "id": request["id"], "result": result})

    fixture = ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    worker = threading.Thread(target=fixture.serve_forever, daemon=True)
    worker.start()
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]

    with tempfile.TemporaryDirectory(prefix="zeroclaw-events-smoke-") as temp:
        root = Path(temp)
        sop = root / "sops" / "review"
        sop.mkdir(parents=True)
        (sop / "SOP.toml").write_text('''[sop]
name = "review"
description = "Loopback event fixture"
version = "1.0.0"
agent = "observer"
execution_mode = "deterministic"
deterministic = true
[[triggers]]
type = "channel"
channel = "event_watch"
alias = "fixture"
''')
        (sop / "SOP.md").write_text('''## Steps

1. **Record observation** — Return the fixture event unchanged.
   - kind: capability
   - capability: noop
''')
        (root / "config.toml").write_text(f'''schema_version = 3
[providers.models.openai.fixture]
model = "fixture"
api_key = "fixture-not-a-real-key"
uri = "http://127.0.0.1:{fixture.server_port}/v1"
wire_api = "chat_completions"
[agents.observer]
model_provider = "openai.fixture"
risk_profile = "observer"
channels = ["event_watch.fixture"]
mcp_bundles = ["fixture"]
[risk_profiles.observer]
level = "readonly"
[channels]
session_persistence = false
[channels.event_watch.fixture]
enabled = true
source = "home_assistant"
mcp_server = "fixture"
sop = "review"
entities = ["light.example"]
states = ["unavailable"]
poll_interval_secs = 10
for_secs = 0
cooldown_secs = 0
[[mcp.servers]]
name = "fixture"
transport = "http"
url = "http://127.0.0.1:{fixture.server_port}/mcp"
headers = {{ Authorization = "Bearer fixture-not-a-real-token" }}
[mcp_bundles.fixture]
servers = ["fixture"]
[gateway]
host = "127.0.0.1"
port = {port}
[sop]
sops_dir = {json.dumps(str(root / "sops"))}
persist_runs = true
step_scope_enforce = true
[heartbeat]
enabled = false
[scheduler]
enabled = false
[wss]
enabled = false
[enroll]
enabled = false
[relay]
enabled = false
''')
        if args.native:
            path = root / "config.toml"
            config = path.read_text().replace('channels = ["event_watch.fixture"]', 'channels = []')
            start = config.index('[channels.event_watch.fixture]')
            end = config.index('[[mcp.servers]]', start)
            config = config[:start] + '[events.sources.ha]\nenabled = true\nkind = "home_assistant"\nmcp_server = "fixture"\npoll_interval_secs = 10\nentities = ["light.example"]\n[events.rules.fixture]\nenabled = true\nsource = "ha"\nsop = "review"\nstates = ["unavailable"]\nfor_secs = 0\ncooldown_secs = 0\n' + config[end:]
            path.write_text(config)
        env = {k: v for k, v in os.environ.items() if not k.startswith("ZEROCLAW_")}
        env["NO_PROXY"] = "127.0.0.1,localhost"
        process = None
        log_path = root / "daemon.log"

        def start():
            return subprocess.Popen([str(binary), "--config-dir", str(root), "daemon"], env=env,
                                    stdout=log, stderr=subprocess.STDOUT)

        def wait_until(check, timeout=45):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise AssertionError("daemon exited: " + diagnostic())
                if check():
                    return
                time.sleep(0.1)
            raise AssertionError("condition timed out: " + diagnostic())

        def diagnostic():
            messages = []
            for path in root.rglob("runtime-trace.jsonl"):
                for line in path.read_text().splitlines()[-150:]:
                    record = json.loads(line)
                    if record.get("severity_number", 0) >= 13:
                        messages.append(record.get("message", ""))
            return "\n".join(messages[-20:]) + "\nUnexpected fixture calls: " + repr(state["unexpected"])

        def runs():
            paths = list(root.rglob("runs.db"))
            if not paths:
                return []
            with sqlite3.connect(paths[0]) as connection:
                return [json.loads(row[0])["run"] for row in connection.execute("SELECT json FROM sop_runs")]

        def baseline_committed():
            for path in root.rglob("events.db"):
                with sqlite3.connect(path) as connection:
                    try:
                        row = connection.execute("SELECT data FROM checkpoint WHERE id=1").fetchone()
                    except sqlite3.OperationalError:
                        continue
                    if row and json.loads(row[0]).get("home", {}).get("light.example", {}).get("state") == "on":
                        return True
            return False

        try:
            with log_path.open("a") as log:
                process = start()
                wait_until(baseline_committed)
                if args.native:
                    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
                    def api(path, body=None, headers=None, method=None):
                        request = urllib.request.Request(f"http://127.0.0.1:{port}" + path,
                            data=None if body is None else json.dumps(body).encode(),
                            headers={"Content-Type":"application/json", **(headers or {})}, method=method)
                        with opener.open(request, timeout=15) as response:
                            return json.load(response)
                    def gateway_ready():
                        try:
                            api("/health")
                            return True
                        except (OSError, urllib.error.URLError):
                            return False
                    wait_until(gateway_ready)
                    try:
                        api("/api/event-monitor")
                        raise AssertionError("anonymous event history was exposed")
                    except urllib.error.HTTPError as error:
                        assert error.code == 401
                    admin = next(root.rglob("gateway-admin.token")).read_text().strip()
                    code = api("/admin/paircode/new", {}, {"x-zeroclaw-admin-token": admin})["pairing_code"]
                    token = api("/pair", {}, {"X-Pairing-Code":code})["token"]
                    auth = {"Authorization":"Bearer " + token}
                    overview = api("/api/event-monitor", headers=auth)
                    assert overview["rules"][0]["last_poll_ms"] is not None
                    assert api("/api/event-monitor/simulate", {"rule":"fixture","state":"on","duration_secs":100}, auth)["matched"] is False
                    assert api("/api/event-monitor/simulate", {"rule":"fixture","state":"unavailable","duration_secs":100}, auth)["matched"] is True
                    assert not runs(), "simulation started a SOP"
                    overview["config"]["sources"]["ha"]["enabled"] = False
                    api("/api/event-monitor", {"config":overview["config"],"revision":overview["revision"]}, auth, "PUT")
                    time.sleep(3)
                    paused_reads = state["reads"]
                    time.sleep(11)
                    assert state["reads"] == paused_reads, "paused source kept polling"
                    fresh = api("/api/event-monitor", headers=auth)
                    fresh["config"]["sources"]["ha"]["enabled"] = True
                    api("/api/event-monitor", {"config":fresh["config"],"revision":fresh["revision"]}, auth, "PUT")
                    wait_until(lambda: state["reads"] > paused_reads)
                warmed_up_calls = state["model_calls"]
                assert not runs(), "baseline unexpectedly started a run"
                state["value"] = "unavailable"
                wait_until(lambda: any(r.get("status") == "completed" for r in runs()))
                assert len(runs()) == 1
                if args.native:
                    records = api("/api/event-monitor", headers=auth)["records"]
                    assert len(records) == 1
                    assert records[0]["run_id"] == runs()[0]["run_id"]
                    assert records[0]["run_status"] == "completed"
                assert state["model_calls"] == warmed_up_calls, "event observer unexpectedly called a model"
                process.terminate()
                process.wait(timeout=15)
                before = state["reads"]
                process = start()
                # The second read proves the first post-restart cycle finished;
                # observing request arrival alone races its eventual dispatch.
                wait_until(lambda: state["reads"] >= before + 2)
                assert len(runs()) == 1, "restart replayed a delivered event"
                assert not state["unexpected"], state["unexpected"]
                print("PASS: daemon baseline -> state transition -> completed SOP -> restart without replay")
        except Exception:
            print(diagnostic())
            if log_path.exists():
                print(log_path.read_text()[-5000:])
            raise
        finally:
            if process and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            fixture.shutdown()
            fixture.server_close()


if __name__ == "__main__":
    main()
