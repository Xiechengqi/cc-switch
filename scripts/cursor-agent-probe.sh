#!/usr/bin/env bash
# Cursor AgentService integration probe (Anthropic tools + OpenAI Responses).
# Usage: BASE_URL=http://127.0.0.1:20125/v1 AUTH_HEADER="Bearer ..." ./scripts/cursor-agent-probe.sh
set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:20125/v1}"
AUTH="${AUTH_HEADER:-}"

hdr=(-H "Content-Type: application/json")
if [[ -n "$AUTH" ]]; then
  hdr+=(-H "Authorization: $AUTH")
fi

echo "== Anthropic tools probe =="
anthropic_body='{
  "model": "claude-sonnet-4-20250514",
  "max_tokens": 256,
  "stream": true,
  "tools": [{"name": "Read", "description": "read file", "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}}}],
  "messages": [{"role": "user", "content": "Call Read on README.md only; do not explain."}]
}'
out=$(curl -sS "${hdr[@]}" -X POST "$BASE_URL/messages" -d "$anthropic_body" | head -c 8000 || true)
if echo "$out" | grep -q 'tool_use\|input_json_delta'; then
  echo "PASS: Anthropic stream contains tool_use"
else
  echo "FAIL: no tool_use in Anthropic response"
  echo "$out" | head -c 500
  exit 1
fi

echo "== OpenAI Responses tools probe =="
responses_body='{
  "model": "gpt-5",
  "stream": true,
  "tools": [{"type": "function", "name": "grep", "parameters": {"type": "object", "properties": {"pattern": {"type": "string"}}}}],
  "input": [{"role": "user", "content": "Use grep to search for fn main in src; tool only."}]
}'
out=$(curl -sS "${hdr[@]}" -X POST "$BASE_URL/responses" -d "$responses_body" | head -c 8000 || true)
if echo "$out" | grep -q 'function_call\|response.output_item.added'; then
  echo "PASS: Responses stream contains function_call"
else
  echo "FAIL: no function_call in Responses output"
  echo "$out" | head -c 500
  exit 1
fi

echo "All probes passed."
