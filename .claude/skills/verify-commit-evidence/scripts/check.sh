#!/usr/bin/env bash
set -euo pipefail
skill_dir=$(cd "$(dirname "$0")/.." && pwd)
exec python3 "$skill_dir/scripts/check.py"
