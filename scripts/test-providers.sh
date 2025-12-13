#!/usr/bin/env bash
# Quick manual test script for EAVS providers
# Usage: ./scripts/test-providers.sh [provider] [prompt]

set -e

EAVS_URL="${EAVS_URL:-http://localhost:3000}"
PROVIDER="${1:-default}"
PROMPT="${2:-Hello! Please respond with just 'EAVS test successful' and nothing else.}"

echo "=== EAVS Provider Test ==="
echo "URL: $EAVS_URL"
echo "Provider: $PROVIDER"
echo "Prompt: $PROMPT"
echo ""

# Test 1: Non-streaming request
echo "--- Test 1: Non-streaming chat completion ---"
RESPONSE=$(curl -s "$EAVS_URL/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -H "X-Provider: $PROVIDER" \
  -d "{
    \"model\": \"gpt-4o-mini\",
    \"messages\": [{\"role\": \"user\", \"content\": \"$PROMPT\"}],
    \"stream\": false,
    \"max_tokens\": 50
  }")

echo "Response:"
echo "$RESPONSE" | jq -r '.choices[0].message.content // .error // .'
echo ""

# Test 2: Streaming request
echo "--- Test 2: Streaming chat completion ---"
echo "Streaming response:"
curl -s "$EAVS_URL/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -H "X-Provider: $PROVIDER" \
  -d "{
    \"model\": \"gpt-4o-mini\",
    \"messages\": [{\"role\": \"user\", \"content\": \"Count from 1 to 5, one number per line.\"}],
    \"stream\": true,
    \"max_tokens\": 50
  }" | while read -r line; do
    if [[ "$line" == data:* ]]; then
      data="${line#data: }"
      if [[ "$data" != "[DONE]" ]]; then
        content=$(echo "$data" | jq -r '.choices[0].delta.content // empty' 2>/dev/null)
        if [[ -n "$content" ]]; then
          echo -n "$content"
        fi
      fi
    fi
  done
echo ""
echo ""

# Test 3: Health check
echo "--- Test 3: Health check ---"
curl -s "$EAVS_URL/health" | jq .
echo ""

# Test 4: List providers
echo "--- Test 4: Available providers ---"
curl -s "$EAVS_URL/providers" | jq .
echo ""

echo "=== Tests complete ==="
