#!/usr/bin/env bash
# Diagnose the Launch-broken regression via CDP.
# Run after starting the wrapper with PRIM1_CDP_PORT=9222.

set -e
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
PYTHON_BIN="${PYTHON_BIN:-python}"
CDP="${PRIM1_CDP_TOOL:-$REPO_ROOT/tools/cdp/cdp.py}"

echo "=== 1. CDP targets ==="
"$PYTHON_BIN" "$CDP" targets

echo ""
echo "=== 2. Launch button info (main/claude) ==="
"$PYTHON_BIN" "$CDP" info 'button[data-action="start"][data-session="claude"]'

echo ""
echo "=== 3. Launch button outer HTML ==="
"$PYTHON_BIN" "$CDP" html 'button[data-action="start"][data-session="claude"]'

echo ""
echo "=== 4. Dispatch synthetic click on Launch ==="
"$PYTHON_BIN" "$CDP" click 'button[data-action="start"][data-session="claude"]'

echo ""
echo "=== 5. Ancestor listener trace ==="
"$PYTHON_BIN" "$CDP" listeners 'button[data-action="start"][data-session="claude"]'

echo ""
echo "=== 6. Any console messages (2.5s window) ==="
"$PYTHON_BIN" "$CDP" console-dump
