#!/usr/bin/env python3
"""Check source binding, run the real CLI properties, compare all verdicts."""
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile

skill = Path(__file__).resolve().parents[1]
crate = skill.parents[2]
source = json.loads((skill / "model/source.json").read_text())
for name, expected in source["files"].items():
    actual = hashlib.sha256((crate / name).read_bytes()).hexdigest()
    if actual != expected:
        accepted = source["mutations"].get(actual) if name == "src/evidence.rs" else None
        if not accepted:
            print(f"SOURCE DRIFT: {name}; review coverage before renewing model/source.json")
            sys.exit(2)
        print(f"REAL SOURCE MUTATION: {accepted}")

cache = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "harbor-verify-commit-evidence"
cache.mkdir(parents=True, exist_ok=True)
evidence = Path(tempfile.mkdtemp(prefix="check-", dir=cache))
print(f"Evidence: {evidence}", flush=True)
command = [sys.executable, str(Path.home() / "git/skills/delegation-skills/shared/scripts/preflight.py"),
           "build", "--crate", "agent-traces", "--", "cargo", "test", "--locked",
           "--test", "commit_evidence_properties", "--", "--include-ignored", "--nocapture", "--test-threads=1"]
with (evidence / "cargo.log").open("w") as log:
    proc = subprocess.Popen(command, cwd=crate, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    try:
        status = proc.wait(timeout=150)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        proc.wait()
        print("INFRASTRUCTURE ERROR: 150-second build/run budget exhausted")
        sys.exit(2)
output = (evidence / "cargo.log").read_text()
expected = json.loads((skill / "model/expectations.json").read_text())
observed = dict(re.findall(r"test (generated_\w+) \.\.\. .*?(ok|FAILED)(?:\n|$)", output, re.S))
if set(observed) != set(expected) or status not in (0, 101):
    print(output)
    print(f"INFRASTRUCTURE ERROR: missing/unexpected test results or cargo exit {status}")
    sys.exit(2)
matched = True
for name, expectation in expected.items():
    verdict = "holds" if observed[name] == "ok" else "violates"
    print(f"{name}: {verdict}; expected {expectation}")
    matched &= verdict == expectation
(evidence / "verdicts.json").write_text(json.dumps(observed, indent=2) + "\n")
if not matched:
    print(output)
sys.exit(0 if matched else 1)
