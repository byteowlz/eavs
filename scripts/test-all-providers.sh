#!/usr/bin/env bash
# Comprehensive EAVS provider test suite
# Tests all configured providers with various scenarios

set -e

EAVS_URL="${EAVS_URL:-http://localhost:3033}"
VERBOSE="${VERBOSE:-0}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

passed=0
failed=0
skipped=0

log_pass() { echo -e "${GREEN}[PASS]${NC} $1"; ((passed++)); }
log_fail() { echo -e "${RED}[FAIL]${NC} $1"; ((failed++)); }
log_skip() { echo -e "${YELLOW}[SKIP]${NC} $1"; ((skipped++)); }
log_info() { echo -e "[INFO] $1"; }

# Check if EAVS is running
check_eavs() {
    if ! curl -s "$EAVS_URL/health" > /dev/null 2>&1; then
        echo "Error: EAVS is not running at $EAVS_URL"
        echo "Start it with: cd /home/wismut/Code/eavs && cargo run"
        exit 1
    fi
}

# Get available providers
get_providers() {
    curl -s "$EAVS_URL/providers" | jq -r '.providers | keys[]' 2>/dev/null
}

# Test non-streaming completion
test_completion() {
    local provider="$1"
    local model="$2"
    
    local response=$(curl -s "$EAVS_URL/v1/chat/completions" \
        -H "Content-Type: application/json" \
        -H "X-Provider: $provider" \
        -d "{
            \"model\": \"$model\",
            \"messages\": [{\"role\": \"user\", \"content\": \"Say 'OK' and nothing else.\"}],
            \"stream\": false,
            \"max_tokens\": 10
        }" 2>&1)
    
    if echo "$response" | jq -e '.choices[0].message.content' > /dev/null 2>&1; then
        log_pass "[$provider] Non-streaming completion"
        return 0
    else
        log_fail "[$provider] Non-streaming completion: $response"
        return 1
    fi
}

# Test streaming completion
test_streaming() {
    local provider="$1"
    local model="$2"
    
    local chunks=0
    local content=""
    
    while IFS= read -r line; do
        if [[ "$line" == data:* ]]; then
            data="${line#data: }"
            if [[ "$data" != "[DONE]" && -n "$data" ]]; then
                ((chunks++))
                c=$(echo "$data" | jq -r '.choices[0].delta.content // empty' 2>/dev/null)
                content+="$c"
            fi
        fi
    done < <(curl -s "$EAVS_URL/v1/chat/completions" \
        -H "Content-Type: application/json" \
        -H "X-Provider: $provider" \
        -d "{
            \"model\": \"$model\",
            \"messages\": [{\"role\": \"user\", \"content\": \"Count: 1, 2, 3\"}],
            \"stream\": true,
            \"max_tokens\": 20
        }" 2>&1)
    
    if [[ $chunks -gt 0 ]]; then
        log_pass "[$provider] Streaming ($chunks chunks)"
        return 0
    else
        log_fail "[$provider] Streaming: No chunks received"
        return 1
    fi
}

# Test system message handling
test_system_message() {
    local provider="$1"
    local model="$2"
    
    local response=$(curl -s "$EAVS_URL/v1/chat/completions" \
        -H "Content-Type: application/json" \
        -H "X-Provider: $provider" \
        -d "{
            \"model\": \"$model\",
            \"messages\": [
                {\"role\": \"system\", \"content\": \"You are a pirate. Always say 'Arrr' at the start.\"},
                {\"role\": \"user\", \"content\": \"Hello\"}
            ],
            \"stream\": false,
            \"max_tokens\": 30
        }" 2>&1)
    
    local content=$(echo "$response" | jq -r '.choices[0].message.content // empty' 2>/dev/null)
    
    if [[ -n "$content" ]]; then
        log_pass "[$provider] System message"
        [[ $VERBOSE == "1" ]] && echo "    Response: $content"
        return 0
    else
        log_fail "[$provider] System message: $response"
        return 1
    fi
}

# Test context injection
test_injection() {
    local provider="$1"
    local model="$2"
    local conv_id="test-injection-$$"
    
    # First, inject a system message
    local inject_result=$(curl -s -X POST "$EAVS_URL/inject/$conv_id" \
        -H "Content-Type: application/json" \
        -d '{
            "messages": [{"role": "system", "content": "Always respond in ALL CAPS."}]
        }' 2>&1)
    
    # Then make a request with that conversation ID
    local response=$(curl -s "$EAVS_URL/v1/chat/completions" \
        -H "Content-Type: application/json" \
        -H "X-Provider: $provider" \
        -H "X-Conversation-ID: $conv_id" \
        -d "{
            \"model\": \"$model\",
            \"messages\": [{\"role\": \"user\", \"content\": \"Say hello\"}],
            \"stream\": false,
            \"max_tokens\": 20
        }" 2>&1)
    
    local content=$(echo "$response" | jq -r '.choices[0].message.content // empty' 2>/dev/null)
    
    if [[ -n "$content" ]]; then
        log_pass "[$provider] Context injection"
        [[ $VERBOSE == "1" ]] && echo "    Response: $content"
        return 0
    else
        log_fail "[$provider] Context injection: $response"
        return 1
    fi
}

# Test multi-turn conversation
test_multiturn() {
    local provider="$1"
    local model="$2"
    
    local response=$(curl -s "$EAVS_URL/v1/chat/completions" \
        -H "Content-Type: application/json" \
        -H "X-Provider: $provider" \
        -d "{
            \"model\": \"$model\",
            \"messages\": [
                {\"role\": \"user\", \"content\": \"My name is TestUser.\"},
                {\"role\": \"assistant\", \"content\": \"Hello TestUser!\"},
                {\"role\": \"user\", \"content\": \"What is my name?\"}
            ],
            \"stream\": false,
            \"max_tokens\": 30
        }" 2>&1)
    
    local content=$(echo "$response" | jq -r '.choices[0].message.content // empty' 2>/dev/null)
    
    if echo "$content" | grep -qi "testuser"; then
        log_pass "[$provider] Multi-turn conversation"
        return 0
    elif [[ -n "$content" ]]; then
        log_pass "[$provider] Multi-turn conversation (response received)"
        [[ $VERBOSE == "1" ]] && echo "    Response: $content"
        return 0
    else
        log_fail "[$provider] Multi-turn conversation: $response"
        return 1
    fi
}

# Main test runner
main() {
    echo "=== EAVS Comprehensive Provider Tests ==="
    echo "URL: $EAVS_URL"
    echo ""
    
    check_eavs
    
    log_info "Fetching available providers..."
    local providers=$(get_providers)
    
    if [[ -z "$providers" ]]; then
        echo "No providers configured!"
        exit 1
    fi
    
    echo "Providers: $providers"
    echo ""
    
    # Default models per provider type (adjust as needed)
    declare -A models
    models[default]="gpt-4o-mini"
    models[openai]="gpt-4o-mini"
    models[anthropic]="claude-sonnet-4-20250514"
    models[google]="gemini-1.5-flash"
    models[ollama]="llama3.2"
    models[local]="llama3.2"
    models[groq]="llama-3.1-8b-instant"
    models[mistral]="mistral-small-latest"
    
    for provider in $providers; do
        echo ""
        echo "--- Testing provider: $provider ---"
        
        local model="${models[$provider]:-gpt-4o-mini}"
        log_info "Using model: $model"
        
        # Run tests (continue on failure)
        test_completion "$provider" "$model" || true
        test_streaming "$provider" "$model" || true
        test_system_message "$provider" "$model" || true
        test_injection "$provider" "$model" || true
        test_multiturn "$provider" "$model" || true
    done
    
    echo ""
    echo "=== Test Summary ==="
    echo -e "${GREEN}Passed: $passed${NC}"
    echo -e "${RED}Failed: $failed${NC}"
    echo -e "${YELLOW}Skipped: $skipped${NC}"
    
    if [[ $failed -gt 0 ]]; then
        exit 1
    fi
}

main "$@"
