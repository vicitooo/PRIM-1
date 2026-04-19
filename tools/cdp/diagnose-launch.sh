#!/usr/bin/env bash
# Diagnose the Launch-broken regression via CDP.
# Run after starting the wrapper with PRIM1_CDP_PORT=9222.

set -e
CDP="python ./tools/cdp/cdp.py"

echo "=== 1. CDP targets ==="
$CDP targets

echo ""
echo "=== 2. Launch button info (main/claude) ==="
$CDP info 'button[data-action="start"][data-session="claude"]'

echo ""
echo "=== 3. Launch button outer HTML ==="
$CDP html 'button[data-action="start"][data-session="claude"]'

echo ""
echo "=== 4. Dispatch synthetic click on Launch ==="
$CDP click 'button[data-action="start"][data-session="claude"]'

echo ""
echo "=== 5. Ancestor listener trace ==="
$CDP listeners 'button[data-action="start"][data-session="claude"]'

echo ""
echo "=== 6. Any console messages (2.5s window) ==="
$CDP console-dump
