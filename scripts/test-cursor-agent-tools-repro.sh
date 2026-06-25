#!/usr/bin/env bash
# Repro probes for Cursor AgentService tool-bearing requests via cc-switch proxy.
# Usage:
#   BASE=https://route-example.jptokenswitch.cc/v1 \
#   TOKEN=ccrt_... \
#   ./scripts/test-cursor-agent-tools-repro.sh
set -euo pipefail

BASE="${BASE:?set BASE to share or local proxy URL, e.g. https://host/v1}"
TOKEN="${TOKEN:?set TOKEN to Bearer api token}"

echo "== A. Claude + tools + stream =="
curl -N -sS -m 45 -X POST "${BASE%/}/v1/messages" \
  -H "Authorization: Bearer ${TOKEN}" \
  -H "Content-Type: application/json" \
  -H "anthropic-version: 2023-06-01" \
  -d '{"model":"claude-sonnet-4-20250514","max_tokens":256,"stream":true,
       "tools":[{"name":"Bash","description":"bash",
         "input_schema":{"type":"object","properties":{"command":{"type":"string"}}}}],
       "messages":[{"role":"user","content":"run ls"}]}' | head -40

echo
echo "== B. Codex chat + tools + stream =="
curl -N -sS -m 45 -X POST "${BASE%/}/v1/chat/completions" \
  -H "Authorization: Bearer ${TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-5.4","stream":true,
       "tools":[{"type":"function","function":{"name":"Bash","parameters":{"type":"object"}}}],
       "messages":[{"role":"user","content":"run ls"}]}' | head -40
