#!/usr/bin/env python3
"""Exercise the installed daemon boundary using loopback fixtures only.

Usage: python3 scripts/ci/event_watch_smoke.py --binary target/debug/zeroclaw
Build with channel-event-watch and gateway. No real accounts or external model calls.
"""
import argparse
import base64
import select
import struct
import hashlib
from datetime import datetime, timezone
import json
import os
import socket
import sqlite3
import subprocess
import tempfile
import threading
import tomllib
import time
import urllib.request
import urllib.error
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--native", action="store_true")
    parser.add_argument("--plugin", action="store_true", help="Exercise the Fastmail integration package")
    parser.add_argument("--push", action="store_true", help="Exercise JMAP SSE wake-ups without mail polling")
    parser.add_argument("--home-push", action="store_true", help="Exercise filtered Home Assistant entity push")
    args = parser.parse_args()
    if args.home_push:
        args.plugin = True
    if args.push:
        args.plugin = True
    if args.plugin:
        args.native = True
    binary = args.binary.resolve()
    state = {"value": "on", "reads": 0, "model_calls": 0, "unexpected": [], "push_revision":0, "push_connections":0, "disconnect":0, "stop":False, "rest_reads":0}

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

        def home_websocket(self):
            key = self.headers["Sec-WebSocket-Key"]
            accept = base64.b64encode(hashlib.sha1((key+"258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
            self.send_response(101)
            self.send_header("Upgrade", "websocket")
            self.send_header("Connection", "Upgrade")
            self.send_header("Sec-WebSocket-Accept", accept)
            self.end_headers()
            def send(value):
                payload = json.dumps(value).encode()
                prefix = bytes([0x81, len(payload)]) if len(payload)<126 else bytes([0x81,126])+struct.pack("!H",len(payload))
                self.connection.sendall(prefix+payload)
            def exact(n):
                out=b""
                while len(out)<n:
                    chunk=self.connection.recv(n-len(out))
                    if not chunk: raise EOFError()
                    out+=chunk
                return out
            def receive():
                head=exact(2)
                if head[0]&15 == 8: raise EOFError()
                assert head[0]&15 == 1 and head[1]&128, "expected masked client text"
                size=head[1]&127
                if size==126: size=struct.unpack("!H",exact(2))[0]
                if size==127: size=struct.unpack("!Q",exact(8))[0]
                assert size<=65536
                mask=exact(4);body=exact(size)
                return json.loads(bytes(v^mask[i%4] for i,v in enumerate(body)))
            try:
                send({"type":"auth_required"})
                auth=receive();assert auth=={"type":"auth","access_token":"fixture-not-a-real-token"}
                send({"type":"auth_ok"})
                sub=receive();assert sub=={"id":1,"type":"subscribe_entities","entity_ids":["light.example"]}
                send({"id":1,"type":"result","success":True,"result":None})
                current=state["value"]
                send({"id":1,"type":"event","event":{"a":{"light.example":{"s":current,"a":{"private":"drop attributes"}},"camera.private":{"s":"on"}}}})
                state["reads"]+=1;state["push_connections"]+=1
                disconnected=state["disconnect"]
                while not state["stop"] and state["disconnect"]==disconnected:
                    if current!=state["value"]:
                        current=state["value"]
                        send({"id":1,"type":"event","event":{"c":{"light.example":{"+":{"s":current}}}}})
                        state["reads"]+=1
                    if select.select([self.connection],[],[],0.05)[0]:
                        msg=receive();assert msg["type"]=="ping", "only heartbeat commands are allowed after subscription"
                        send({"id":msg["id"],"type":"pong"})
                self.connection.shutdown(socket.SHUT_RDWR)
            except (EOFError, BrokenPipeError, ConnectionResetError):
                pass

        def do_GET(self):
            if args.home_push and self.path == "/api/websocket":
                self.home_websocket()
                return
            if args.push and self.path == "/jmap/session":
                assert self.headers.get("Authorization") == "Bearer fixture-push-token"
                self.reply({"primaryAccounts":{"urn:ietf:params:jmap:mail":"fixture"}, "eventSourceUrl":f"http://127.0.0.1:{self.server.server_port}/jmap/event"})
                return
            if args.push and self.path.startswith("/jmap/event?"):
                assert self.headers.get("Authorization") == "Bearer fixture-push-token"
                state["push_connections"] += 1
                disconnected = state["disconnect"]
                last = state["push_revision"]
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Connection", "close")
                self.end_headers()
                try:
                    while not state["stop"] and state["disconnect"] == disconnected:
                        if last != state["push_revision"]:
                            last = state["push_revision"]
                            body = json.dumps({"@type":"StateChange", "changed":{"fixture":{"Email":str(last)}}})
                            self.wfile.write(("event:state\ndata:"+body+"\n\n").encode())
                        else:
                            self.wfile.write(b'event:ping\ndata:{"interval":1}\n\n')
                        self.wfile.flush()
                        time.sleep(0.25)
                except (BrokenPipeError, ConnectionResetError):
                    pass
                return
            if self.path == "/v1/models":
                self.reply({"data": [{"id": "fixture"}]})
                return
            if self.path == "/api/states/light.example":
                state["rest_reads"] += 1
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
            if self.path not in ("/mcp", "/api/mcp"):
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
                result = {"tools": [{"name":"homeassistant__GetLiveContext" if args.home_push else "search_email", "description":"Fixture metadata", "inputSchema":{"type":"object"}}] if args.plugin else []}
            elif method == "tools/call" and args.plugin and not args.home_push:
                assert request["params"]["name"] == "search_email"
                assert request["params"]["arguments"]["query"] == "in:inbox"
                state["reads"] += 1
                messages = [{"id":"baseline", "receivedAt":"2026-01-01T00:00:00Z", "subject":"Baseline fixture"}]
                if state["value"] == "unavailable":
                    messages.append({"id":"new-message", "receivedAt":datetime.now(timezone.utc).isoformat(), "subject":"Action fixture"})
                result = {"structuredContent":{"results":messages, "hasMore":False}}
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
            if args.plugin and not args.home_push:
                package = root / "shared/integration-plugins/fastmail"
                package.mkdir(parents=True)
                manifest = (Path(__file__).resolve().parents[2] / "crates/zeroclaw-config/integrations/fastmail/integration.toml").read_text().replace("https://api.fastmail.com", f"http://127.0.0.1:{fixture.server_port}").replace("https://phl.api.fastmail.com", f"http://127.0.0.1:{fixture.server_port}")
                (package / "integration.toml").write_text(manifest)
                digest = hashlib.sha256(manifest.encode()).hexdigest()
                config = config.replace('[risk_profiles.observer]', '[risk_profiles.observer]\nauto_approve = ["fixture__search_email"]')
                start = config.index('[events.sources.ha]')
                end = config.index('[[mcp.servers]]', start)
                config = config[:start] + f'''[integration_plugins.instances.mail]
enabled = true
package = "fastmail"
manifest_sha256 = "{digest}"
mcp_server = "fixture"
[events.sources.mail]
enabled = true
kind = "plugin"
plugin = "mail"
poll_interval_secs = 10
[events.rules.fixture]
enabled = true
source = "mail"
sop = "review"
''' + config[end:]
            if args.push:
                config = config.replace('[integration_plugins.instances.mail]', '[integration_plugins.instances.mail]\npush_enabled = true\npush_token = "fixture-push-token"')
            if args.home_push:
                package = root / "shared/integration-plugins/homeassistant"
                package.mkdir(parents=True)
                manifest = (Path(__file__).resolve().parents[2] / "crates/zeroclaw-config/integrations/homeassistant/integration.toml").read_text()
                (package / "integration.toml").write_text(manifest)
                digest = hashlib.sha256(manifest.encode()).hexdigest()
                config = config.replace('[risk_profiles.observer]', '[risk_profiles.observer]\nauto_approve = ["fixture__homeassistant__GetLiveContext"]')
                config = config.replace(f'url = "http://127.0.0.1:{fixture.server_port}/mcp"', f'url = "http://127.0.0.1:{fixture.server_port}/api/mcp"')
                start=config.index('[events.sources.ha]');end=config.index('[[mcp.servers]]',start)
                config=config[:start]+f'''[integration_plugins.instances.homeassistant]
enabled = true
package = "homeassistant"
manifest_sha256 = "{digest}"
mcp_server = "fixture"
push_enabled = true
[events.sources.ha]
enabled = true
kind = "plugin"
plugin = "homeassistant"
entities = ["light.example"]
poll_interval_secs = 10
[events.rules.fixture]
enabled = true
source = "ha"
sop = "review"
states = ["unavailable"]
for_secs = 1
cooldown_secs = 0
'''+config[end:]
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
                    if row:
                        checkpoint = json.loads(row[0])
                        if args.plugin and not args.home_push and checkpoint.get("mail_boundary") is not None:
                            return True
                        if (not args.plugin or args.home_push) and checkpoint.get("home", {}).get("light.example", {}).get("state") == "on":
                            return True
            return False

        try:
            with log_path.open("a") as log:
                process = start()
                wait_until(baseline_committed)
                if args.push or args.home_push:
                    idle_reads = state["reads"]
                    time.sleep(11)
                    assert state["reads"] == idle_reads, "idle SSE connection polled mail"
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
                    assert api("/api/event-monitor/simulate", {"rule":"fixture","state":"on","duration_secs":100}, auth)["matched"] is (args.plugin and not args.home_push)
                    assert api("/api/event-monitor/simulate", {"rule":"fixture","state":"unavailable","duration_secs":100}, auth)["matched"] is True
                    assert not runs(), "simulation started a SOP"
                    endpoint = "/api/integration-plugins" if args.plugin else "/api/event-monitor"
                    setting = "instances" if args.plugin else "sources"
                    instance = "homeassistant" if args.home_push else ("mail" if args.plugin else "ha")
                    overview = api(endpoint, headers=auth)
                    if args.push:
                        assert overview["config"]["instances"]["mail"]["push_token"] is None
                        assert "fixture-push-token" not in json.dumps(overview)
                    overview["config"][setting][instance]["enabled"] = False
                    api(endpoint, {"config":overview["config"],"revision":overview["revision"]}, auth, "PUT")
                    saved = tomllib.loads((root / "config.toml").read_text())
                    section = "integration_plugins" if args.plugin else "events"
                    assert saved[section][setting][instance]["enabled"] is False, "pause was not persisted"
                    if args.push:
                        assert saved[section][setting][instance]["push_token"].startswith("enc2:"), "push credential was not encrypted"
                    time.sleep(3)
                    paused_reads = state["reads"]
                    time.sleep(11)
                    assert state["reads"] == paused_reads, "paused source kept polling"
                    fresh = api(endpoint, headers=auth)
                    fresh["config"][setting][instance]["enabled"] = True
                    api(endpoint, {"config":fresh["config"],"revision":fresh["revision"]}, auth, "PUT")
                    saved = tomllib.loads((root / "config.toml").read_text())
                    assert saved[section][setting][instance]["enabled"] is True, "resume was not persisted"
                    wait_until(lambda: state["reads"] > paused_reads)
                warmed_up_calls = state["model_calls"]
                assert not runs(), "baseline unexpectedly started a run"
                state["value"] = "unavailable"
                pushed_at = time.monotonic()
                state["push_revision"] += 1
                wait_until(lambda: any(r.get("status") == "completed" for r in runs()))
                assert len(runs()) == 1
                if args.push or args.home_push:
                    assert time.monotonic()-pushed_at < 5, "SSE did not promptly wake the observer"
                if args.native:
                    records = api("/api/event-monitor", headers=auth)["records"]
                    assert len(records) == 1
                    assert records[0]["run_id"] == runs()[0]["run_id"]
                    assert records[0]["run_status"] == "completed"
                assert state["model_calls"] == warmed_up_calls, "event observer unexpectedly called a model"
                if args.push or args.home_push:
                    connected = state["push_connections"]
                    reads_before_disconnect = state["reads"]
                    state["disconnect"] += 1
                    wait_until(lambda: state["push_connections"] > connected and state["reads"] > reads_before_disconnect)
                    assert len(runs()) == 1, "reconnect replayed a delivered event"
                process.terminate()
                process.wait(timeout=15)
                before = state["reads"]
                process = start()
                # The second read proves the first post-restart cycle finished;
                # observing request arrival alone races its eventual dispatch.
                wait_until(lambda: state["reads"] >= before + (1 if args.push or args.home_push else 2))
                if args.push or args.home_push:
                    time.sleep(3)
                assert len(runs()) == 1, "restart replayed a delivered event"
                assert not state["unexpected"], state["unexpected"]
                if args.plugin:
                    package_file = root / "shared/integration-plugins" / ("homeassistant" if args.home_push else "fastmail") / "integration.toml"
                    package_file.write_text(package_file.read_text() + "\n# unapproved change\n")
                    time.sleep(3)
                    paused_reads = state["reads"]
                    time.sleep(11)
                    assert state["reads"] == paused_reads, "unapproved package kept polling"
                    catalog = api("/api/integration-plugins", headers=auth)
                    assert ("homeassistant" if args.home_push else "mail") in catalog["invalid_instances"]
                if args.home_push:
                    assert state["rest_reads"] == 0, "Home Assistant push performed REST state polling"
                print("PASS: daemon baseline -> pause/resume -> event -> completed SOP -> restart without replay" + (" -> tamper stops observation" if args.plugin else "") + (" (SSE: no idle polling, prompt wake-up, reconnect, secret redaction)" if args.push else "") + (" (HA WebSocket: scoped snapshot, no REST polling, duration timer, reconnect)" if args.home_push else ""))
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
            state["stop"] = True
            fixture.shutdown()
            fixture.server_close()


if __name__ == "__main__":
    main()
