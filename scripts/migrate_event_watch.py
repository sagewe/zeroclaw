#!/usr/bin/env python3
"""Offline migration of one legacy event watch. Stop the daemon first.

Backs up config and copies the checkpoint with SQLite backup; never deletes the
legacy store. Refuses ambiguous text layouts and never reads credentials into
stdout. The caller supplies the legacy DB path because agent workspaces can be
customized. Python 3.11+, no extra dependencies.
"""
import argparse
import datetime
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import tomllib


def section_value(value):
    if isinstance(value, bool):
        return str(value).lower()
    if isinstance(value, (str, int, float, list)):
        return json.dumps(value, ensure_ascii=False)
    raise ValueError("unsupported migration value")


def migrate(root, rule, source, legacy_db):
    if not all(re.fullmatch(r"[A-Za-z0-9_-]{1,64}", key) for key in [rule, source]):
        raise ValueError("invalid rule or source ID")
    path = root / "config.toml"
    text = path.read_text()
    config = tomllib.loads(text)
    watch = config["channels"]["event_watch"][rule]
    events = config.get("events", {})
    if source in events.get("sources", {}) or rule in events.get("rules", {}):
        raise ValueError("native source or rule already exists")
    pattern = r"(?ms)^\[channels\.event_watch\." + re.escape(rule) + r"\]\n.*?(?=^\[|\Z)"
    text, count = re.subn(pattern, "", text)
    if count != 1:
        raise ValueError("ambiguous legacy table; migrate manually")
    for agent, value in config.get("agents", {}).items():
        binding = "event_watch." + rule
        if binding not in value.get("channels", []):
            continue
        pattern = r"(?ms)(^\[agents\." + re.escape(agent) + r"\]\n)(.*?)(?=^\[|\Z)"
        match = re.search(pattern, text)
        if not match:
            raise ValueError("ambiguous agent table; migrate manually")
        channels = [c for c in value["channels"] if c != binding]
        body, count = re.subn(r"(?ms)^channels\s*=\s*\[[^\]]*\]", "channels = " + json.dumps(channels), match[2])
        if count != 1:
            raise ValueError("ambiguous channel bindings")
        text = text[:match.start(2)] + body + text[match.end(2):]
    source_fields = {k: watch[k] for k in ["enabled", "mcp_server", "poll_interval_secs", "query", "entities"] if k in watch}
    source_fields["kind"] = watch["source"]
    rule_fields = {k: watch[k] for k in ["enabled", "sop", "states", "above", "below", "for_secs", "cooldown_secs"] if k in watch}
    rule_fields["source"] = source
    for table, fields in [("sources." + source, source_fields), ("rules." + rule, rule_fields)]:
        text += "\n[events." + table + "]\n" + "".join(k + " = " + section_value(v) + "\n" for k, v in fields.items())
    parsed = tomllib.loads(text)
    assert rule not in parsed.get("channels", {}).get("event_watch", {})
    backup = root / "backups" / ("events-console-" + datetime.datetime.now().strftime("%Y%m%d-%H%M%S-%f"))
    backup.mkdir(parents=True, mode=0o700)
    shutil.copy2(path, backup / "config.toml")
    (backup / "config.toml").chmod(0o600)
    data_dir = Path(config.get("data_dir", root / "data")).expanduser()
    if not data_dir.is_absolute():
        raise ValueError("relative data_dir requires manual migration")
    target = data_dir / "events" / rule
    if target.exists():
        raise ValueError("destination already exists")
    if not legacy_db.is_file() or legacy_db.is_symlink():
        raise ValueError("legacy database missing or symlinked")
    target.mkdir(parents=True, mode=0o700)
    os.chmod(target, 0o700)
    # SQLite backup is safe for checkpoints even if an old journal is present.
    with sqlite3.connect(legacy_db.as_uri() + "?mode=ro", uri=True) as old:
        with sqlite3.connect(target / "events.db") as new:
            old.backup(new)
    (target / "events.db").chmod(0o600)
    stage = path.with_suffix(".toml.events-new")
    fd = os.open(stage, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as handle:
        handle.write(text)
    stage.replace(path)
    return backup, target


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config-root", type=Path, required=True)
    parser.add_argument("--rule", required=True)
    parser.add_argument("--source", required=True)
    parser.add_argument("--legacy-db", type=Path, required=True)
    parser.add_argument("--daemon-stopped", action="store_true", required=True)
    args = parser.parse_args()
    backup, target = migrate(args.config_root.resolve(), args.rule, args.source, args.legacy_db.absolute())
    print("Configuration backup:", backup)
    print("Migrated checkpoint:", target)
