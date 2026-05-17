#!/usr/bin/env bash
# capture-pane-ids.sh — detect the underlying CLI session IDs of the 4 live PRIM-1 panes.
#
# WHY: the wrapper tracks panes by NAME, not by underlying claude/codex session ID. To
# resume a pane after a machine restart you need its session ID:
#   control-plane.ps1 -Action start -Session <name> -ExtraArgs '--resume','<session-id>'
# This script finds the current IDs by scanning the on-disk session files + matching the
# pane-prime signature. Codex pane IDs DRIFT (codex rolls a new file on compaction), so
# re-run this right before any restart-to-resume.
#
# Usage:  bash capture-pane-ids.sh
# Output: the 4 pane->session-id mappings, newest first.

set -euo pipefail

CLAUDE_PROJ="C:/Users/alice/.claude/projects/D--All-Repos-sample"
CODEX_ROOT="C:/Users/alice/.codex/sessions"
SUPERVISOR_SESSION="00000000-0000-4000-8000-000000000003"   # outside-supervisor — exclude

echo "=== PRIM-1 pane session IDs — $(date -u +%Y-%m-%dT%H:%M:%SZ) ==="
echo ""

# --- Claude-driver panes: claude (inside-supervisor) + FrontendQA-claude ---
for role in "CLAUDE-pane" "FRONTENDQA-claude"; do
  found=""
  while IFS= read -r f; do
    id=$(basename "$f" .jsonl)
    [ "$id" = "$SUPERVISOR_SESSION" ] && continue
    if head -c 60000 "$f" 2>/dev/null | grep -q "You are $role"; then
      found="$id"
      break
    fi
  done < <(ls -t "$CLAUDE_PROJ"/*.jsonl 2>/dev/null)
  case "$role" in
    "CLAUDE-pane")        echo "claude              $found" ;;
    "FRONTENDQA-claude")  echo "FrontendQA-claude   $found" ;;
  esac
done

# --- Codex-driver panes: codex (builder) + FrontendQA-codex ---
# Scan the two most recent date dirs (sessions roll by UTC date).
CODEX_FILES=$(find "$CODEX_ROOT" -name "rollout-*.jsonl" -newermt "12 hours ago" 2>/dev/null \
              | xargs ls -t 2>/dev/null || true)
for role in "CODEX-pane" "FRONTENDQA-codex"; do
  found=""
  while IFS= read -r f; do
    [ -z "$f" ] && continue
    fn=$(basename "$f" .jsonl)
    id=$(echo "$fn" | sed -E 's/rollout-[0-9T-]+-([0-9a-f-]+)$/\1/')
    if grep -q "You are $role" "$f" 2>/dev/null; then
      found="$id"
      break
    fi
  done <<< "$CODEX_FILES"
  case "$role" in
    "CODEX-pane")        echo "codex               $found" ;;
    "FRONTENDQA-codex")  echo "FrontendQA-codex    $found" ;;
  esac
done

echo ""
echo "Resume a pane:  control-plane.ps1 -Action stop -Session <name>"
echo "                control-plane.ps1 -Action start -Session <name> -ExtraArgs '--resume','<id>'"
echo "NOTE: codex IDs drift on compaction — re-run this immediately before any restart-to-resume."
