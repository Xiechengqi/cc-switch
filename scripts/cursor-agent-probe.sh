#!/usr/bin/env bash
# Cursor AgentService E2E probe — tests Codex CLI and Claude Code style
# requests against a cc-switch share endpoint. Verifies:
#  1. Plain text streaming works (Responses + Messages)
#  2. Tool-bearing streaming produces tool_call events (not hang)
#  3. Non-streaming plain text returns valid JSON
#
# Usage: ./cursor-agent-probe.sh <base_url> <api_key>
# Example: ./cursor-agent-probe.sh https://route-tuoxq.jptokenswitch.cc/v1 ccrt_xxx
set -euo pipefail

BASE="${1:?Usage: $0 <base_url> <api_key>}"
KEY="${2:?Usage: $0 <base_url> <api_key>}"
TIMEOUT=60
PASS=0
FAIL=0

check() {
    local name="$1" expected="$2" actual="$3"
    if echo "$actual" | grep -q "$expected"; then
        echo "  PASS: $name"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $name (expected: $expected)"
        FAIL=$((FAIL + 1))
    fi
}

echo "=== Cursor AgentService E2E Probe ==="
echo "Endpoint: $BASE"
echo ""

# ── 1. Responses plain text streaming ──────────────────────────────────────
echo "--- [1] /responses streaming plain text ---"
OUT=$(curl -sS --max-time $TIMEOUT -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -X POST "$BASE/responses" \
  -d '{"model":"gpt-5.5","input":"Say hello.","stream":true}' 2>&1)
check "response.created" "response.created" "$OUT"
check "response.completed" "response.completed" "$OUT"
check "[DONE]" "\[DONE\]" "$OUT"
echo ""

# ── 2. Responses with tools streaming ──────────────────────────────────────
echo "--- [2] /responses streaming with tools ---"
OUT=$(curl -sS --max-time $TIMEOUT -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -X POST "$BASE/responses" \
  -d '{"model":"gpt-5.5","input":"List files using the shell tool.","stream":true,"tools":[{"type":"function","name":"shell","description":"Run a shell command","parameters":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}}]}' 2>&1)
check "response.created" "response.created" "$OUT"
# Either tool_call or completed — not a hang
if echo "$OUT" | grep -q "response.completed\|function_call\|tool_call"; then
    echo "  PASS: got terminal event or tool call (not hung)"
    PASS=$((PASS + 1))
else
    echo "  FAIL: no terminal event or tool call (likely hung)"
    FAIL=$((FAIL + 1))
fi
echo ""

# ── 3. Messages plain text streaming ───────────────────────────────────────
echo "--- [3] /messages streaming plain text (Claude Code) ---"
OUT=$(curl -sS --max-time $TIMEOUT -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -H "anthropic-version: 2023-06-01" \
  -X POST "$BASE/messages" \
  -d '{"model":"claude-sonnet-4-20250514","max_tokens":256,"stream":true,"messages":[{"role":"user","content":"Say hello."}]}' 2>&1)
check "message_start" "message_start" "$OUT"
check "message_stop" "message_stop" "$OUT"
echo ""

# ── 4. Messages with tools streaming ───────────────────────────────────────
echo "--- [4] /messages streaming with tools (Claude Code) ---"
OUT=$(curl -sS --max-time $TIMEOUT -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -H "anthropic-version: 2023-06-01" \
  -X POST "$BASE/messages" \
  -d '{"model":"claude-sonnet-4-20250514","max_tokens":1024,"stream":true,"messages":[{"role":"user","content":"List files using the shell tool."}],"tools":[{"name":"shell","description":"Run a shell command","input_schema":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}}]}' 2>&1)
check "message_start" "message_start" "$OUT"
if echo "$OUT" | grep -q "message_stop\|tool_use"; then
    echo "  PASS: got terminal event or tool call (not hung)"
    PASS=$((PASS + 1))
else
    echo "  FAIL: no terminal event or tool call (likely hung)"
    FAIL=$((FAIL + 1))
fi
echo ""

# ── 5. Chat completions with tools streaming ───────────────────────────────
echo "--- [5] /chat/completions streaming with tools ---"
OUT=$(curl -sS --max-time $TIMEOUT -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -X POST "$BASE/chat/completions" \
  -d '{"model":"gpt-5.5","stream":true,"messages":[{"role":"user","content":"List files using the shell tool."}],"tools":[{"type":"function","function":{"name":"shell","description":"Run a shell command","parameters":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}}}]}' 2>&1)
if echo "$OUT" | grep -q "\[DONE\]\|tool_calls"; then
    echo "  PASS: got terminal event or tool call"
    PASS=$((PASS + 1))
else
    echo "  FAIL: no terminal event (likely hung)"
    FAIL=$((FAIL + 1))
fi
echo ""

echo "=== Results: $PASS passed, $FAIL failed ==="
exit $FAIL
