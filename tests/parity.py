#!/usr/bin/env python3
"""Compare the native CLI with stored outputs on disposable fixtures."""
import argparse
import difflib
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from datetime import datetime, timezone

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("corpus", ROOT / "tests/fixtures/corpus.py")
corpus = importlib.util.module_from_spec(spec)
spec.loader.exec_module(corpus)


def environment(home):
    env = dict(os.environ)
    for key in ("CLAUDE_CONFIG_DIR", "CODEX_HOME", "XDG_DATA_HOME", "CLAUDE_CODE_SESSION_ID", "CODEX_THREAD_ID", "CODEX_SESSION_ID"):
        env.pop(key, None)
    env.update(AGENT_TRACES_HOME=str(home), HOME=str(home), USERPROFILE=str(home),
               AGENT_TRACES_CACHE=str(home / "cache"), AGENT_TRACES_OUT=str(home / "out"),
               AGENT_TRACES_NOW=corpus.NOW, AGENT_TRACES_HOST_ID="fixture-host", AGENT_TRACES_HOST="fixture-host",
               AGENT_TRACES_PROCESS_SNAPSHOT=str(home / "processes.json"),
               PYTHONIOENCODING="utf-8", PYTHONUTF8="1", TZ="UTC")
    return env


def normalize(value, home, field=None):
    if isinstance(value, dict):
        return {k: normalize(v, home, k) for k, v in value.items()}
    if isinstance(value, list):
        return [normalize(v, home) for v in value]
    if isinstance(value, str):
        value = value.replace(str(home), "<HOME>")
        if os.name == "nt":
            # Windows only: CRLF, and backslashes in paths under the home, absolute or as "~\\...".
            # Goldens are generated on Linux and hold "\n" and "/".
            value = value.replace("\r\n", "\n")
            value = re.sub(r'(?:<HOME>|(?<![\w~.\\/])~(?=\\))[^\s"\',;()]*', lambda m: m[0].replace("\\", "/"), value)
        # Only equivalent timestamp spelling; no dropped keys or reordered lists.
        if field in {"started", "updated", "ts", "observed_at", "process_started", "last_activity"} and len(value) >= 20 and value[4:5] == "-" and "T" in value:
            try:
                return datetime.fromisoformat(value.replace("Z", "+00:00")).astimezone(timezone.utc).isoformat()
            except ValueError:
                pass
    return value


def snapshot(home):
    return {str(p.relative_to(home)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in home.rglob("*") if p.is_file()
            and p.relative_to(home).parts[0].startswith(".")
            and not p.name.endswith(("-shm", "-wal"))}


def invoke(cmd, env):
    cwd = Path(env["AGENT_TRACES_HOME"]) / "project"
    cwd.mkdir(parents=True, exist_ok=True)
    return subprocess.run(cmd, env=env, cwd=cwd, capture_output=True, text=True, encoding="utf-8", timeout=60)


class GoldenStore:
    def __init__(self, filename, update=False):
        self.path = ROOT / "tests/golden" / filename
        self.update = update
        self.entries = [] if update else json.loads(self.path.read_text(encoding="utf-8"))
        self.by_id = {entry["id"]: entry for entry in self.entries}
        if len(self.by_id) != len(self.entries):
            raise AssertionError("duplicate golden IDs: " + filename)
        self.ids = []

    def register(self, label):
        self.ids.append(label)

    def check(self, label, args, kind, exit_code, output, home):
        expected = ([normalize(json.loads(line), home) for line in output.splitlines()]
                    if kind == "json" else normalize(output, home))
        entry = dict(id=label, args=[normalize(arg, home) for arg in args],
                     kind=kind, exit=exit_code, expected=expected)
        if self.update:
            self.entries.append(entry)
        elif self.by_id.get(label) != entry:
            print("FAIL", label, "golden mismatch")
            left = json.dumps(self.by_id.get(label), indent=2, ensure_ascii=False, sort_keys=True)
            right = json.dumps(entry, indent=2, ensure_ascii=False, sort_keys=True)
            print("".join(difflib.unified_diff(left.splitlines(True), right.splitlines(True), fromfile="golden", tofile="actual")))
            return False
        print("PASS", label)
        return True

    def finish(self):
        if not self.update and set(self.ids) != set(self.by_id):
            raise AssertionError("golden case set mismatch: " + repr(set(self.ids) ^ set(self.by_id)))
        if self.update:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            self.path.write_text(json.dumps(self.entries, indent=2, ensure_ascii=False, sort_keys=True) + "\n", encoding="utf-8")


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--binary", type=Path)
    ap.add_argument("--update-golden", action="store_true", help="rewrite stored expectations from the binary")
    ap.add_argument("--schema", action="store_true", help="validate v1 with optional jsonschema dev package")
    ap.add_argument("--case", help="run cases whose command contains this substring")
    opts = ap.parse_args()
    if opts.update_golden and opts.case is not None:
        ap.error("--update-golden cannot be combined with --case")
    if opts.update_golden and os.name == "nt":
        ap.error("--update-golden runs on Linux or macOS; goldens hold POSIX paths and line endings")
    binary = (opts.binary or ROOT / "target/debug" / ("agent-traces.exe" if os.name == "nt" else "agent-traces")).resolve()
    with tempfile.TemporaryDirectory(prefix="agent-traces-parity-") as temp:
        home = Path(temp).resolve()  # the real path: macOS temp dirs sit behind a /private link
        ids = corpus.create(home)
        identity = ids.get("claude") or next(iter(ids.values()))
        (home / "processes.json").write_text("[]", encoding="utf-8")
        env = environment(home)
        before = snapshot(home)
        cases = [(["where"], False), (["ls", "--all", "--json"], True), (["ls", "--all", "--subagents", "--json"], True),
                 (["find", "mailbox", "--json", "--include-self"], True),
                 (["find", "mailbox", "routing", "--json", "--include-self", "--sort", "oldest"], True),
                 (["find", "absent-needle-xyz", "--json"], True),
                 (["live"], False), (["me"], False),
                 (["touched", "router.py", "--since", "2026-09-01"], False),
                 (["failures", "--min", "1", "--since", "2026-09-01"], False)]
        cases += [
            (["find", "mailbox", "--include-self", "--hits", "-1"], False),
            (["find", "mailbox", "--include-self", "--hits", "-100"], False),
            (["touched", "router.py", "--since", "2026-09-01", "--limit", "-1"], False),
            (["touched", "router.py", "--since", "2026-09-01", "--limit", "-100"], False),
            (["failures", "--min", "-1", "--since", "2026-09-01", "--limit", "-1", "--json"], True),
            (["failures", "--min", "-1", "--since", "2026-09-01", "--limit", "-100"], False),
            (["failures", "--min", "-1", "--since", "2026-09-01", "--json"], True),
            (["show", identity, "--tail", "-1", "--full-stdout"], False),
            (["show", identity, "--tail", "-1", "--grep", "mailbox", "--full-stdout"], False),
        ]
        for ident in ids.values():
            cases += [(["resolve", ident, "--json"], True),
                      (["show", ident, "--full-stdout", "--results"], False),
                      (["tree", ident], False)]
        cases += [(["resolve", "no-such-session", "--json"], True),
                  (["handoff", identity, "--output", str(home / "handoff.md")], False)]
        failed = checked = 0
        golden = GoldenStore("cases.json", opts.update_golden)
        for args, is_json in cases:
            label = normalize(" ".join(args), home)
            kind = "json" if is_json else "artifact" if args[0] == "handoff" else "text"
            golden.register(label)
            if opts.case and opts.case not in label:
                continue
            checked += 1
            case_env = dict(env, CLAUDE_CODE_SESSION_ID=identity) if args[0] == "me" else env
            actual = invoke([str(binary), *args], case_env)
            if args[0] == "ls" and len(actual.stdout.splitlines()) != 4:
                raise AssertionError("fixture listing must have four sessions after T3/provider merge")
            output = (home / "handoff.md").read_text(encoding="utf-8") if kind == "artifact" else actual.stdout
            failed += not golden.check(label, args, kind, actual.returncode, output, home)
        golden.finish()
        # CLI and stores work with no Python, rg, or other programs on PATH.
        clean_env = dict(env, PATH="")
        for smoke_args, _ in cases:
            smoke_env = dict(clean_env, CLAUDE_CODE_SESSION_ID=identity) if smoke_args[0] == "me" else clean_env
            smoke = invoke([str(binary), *smoke_args], smoke_env)
            if smoke.returncode not in (0, 1):
                raise AssertionError("self-contained executable smoke failed: " + repr(smoke_args) + smoke.stderr)
        if opts.schema:
            import jsonschema
            schema = json.loads((ROOT / "schemas/cli-v1.schema.json").read_text(encoding="utf-8"))
            identity = next(iter(ids.values()))
            for args in (["ls", "--all"], ["find", "mailbox"], ["live"], ["resolve", identity], ["me"], ["handoff", identity]):
                result = invoke([str(binary), *args, "--json", "--schema-version", "1"], dict(env, CLAUDE_CODE_SESSION_ID=identity))
                envelope = json.loads(result.stdout)
                jsonschema.Draft202012Validator(schema, format_checker=jsonschema.FormatChecker()).validate(envelope)
                expected_exits = {"ok": 0, "not_found": 1, "error": 2, "partial": 3, "ambiguous": 4}
                if result.returncode != expected_exits[envelope["status"]]:
                    raise AssertionError("v1 exit/status mismatch: " + args[0])
                if envelope["status"] not in ("ok", "partial"):
                    raise AssertionError("known fixture must return useful data: " + args[0])
                if envelope["status"] == "partial":
                    codes = {d["code"] for d in envelope["diagnostics"]}
                    if not codes or codes - {"malformed_record"}:
                        raise AssertionError("unexpected fixture partial failure: " + repr(codes))
                data = envelope["result"]
                if args[0] == "live":
                    observations = data["observations"]
                    if envelope["status"] != "ok" or len(observations) != 2:
                        raise AssertionError("fixture has exactly two running T3 observations")
                    observed = {(o["identity"]["id"], o["evidence"], o["observed_status"]) for o in observations}
                    expected = {(i, "t3_runtime", "running") for i in [ids["t3"], "thread-second-fixture"]}
                    if observed != expected:
                        raise AssertionError("wrong live identities/evidence/status")
                elif args[0] == "me":
                    if data["identifiers"] != [{"environment_variable": "CLAUDE_CODE_SESSION_ID", "id": identity, "resolved": True}]:
                        raise AssertionError("me did not resolve the supplied environment identity")
                    if [s["identity"]["id"] for s in data["sessions"]] != [identity]:
                        raise AssertionError("me returned the wrong session")
                elif args[0] == "ls" and data["total"] != 4:
                    raise AssertionError("v1 fixture listing must have four merged sessions")
            with tempfile.TemporaryDirectory(prefix="agent-traces-empty-") as empty:
                empty_home = Path(empty).resolve()
                (empty_home / "processes.json").write_text("[]", encoding="utf-8")
                empty_env = environment(empty_home)
                for command in (["ls", "--all"], ["live"], ["me"], ["resolve", "absent-session"]):
                    result = invoke([str(binary), *command, "--json", "--schema-version", "1"], empty_env)
                    envelope = json.loads(result.stdout)
                    jsonschema.Draft202012Validator(schema, format_checker=jsonschema.FormatChecker()).validate(envelope)
                    expected = (0, "ok") if command[0] == "ls" else (1, "not_found")
                    if (result.returncode, envelope["status"]) != expected or envelope["complete"] is not True:
                        raise AssertionError("empty-store status/exit regression: " + command[0])
                empty_env.pop("AGENT_TRACES_HOST_ID")
                result = invoke([str(binary), "ls", "--json", "--schema-version", "1"], empty_env)
                if result.returncode != 2 or result.stdout:
                    raise AssertionError("v1 must reject a missing host ID before emitting an envelope")
        if before != snapshot(home):
            raise AssertionError("command mutated fixture store content")
        if not opts.case or "handoff" in opts.case:
            from handoff_safety import run_cases
            checked += run_cases(binary, update_golden=opts.update_golden)
        print(f"{checked} cases, {failed} failures")
        return bool(failed)


if __name__ == "__main__":
    sys.exit(main())
