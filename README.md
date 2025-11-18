# EAVS - Enhanced AI Validation System

A local, Rust-based LLM proxy with zero-latency bidirectional streaming, full logging, and live context injection.

## Features

- **Transparent Proxy**: Forwards OpenAI-compatible requests with zero latency.
- **Live Logging**: Logs all requests and streamed responses to an internal broadcast channel.
- **Context Injection**: Pre-request injection of system or user messages via a control API.
- **Control API**: Manage injections and stream logs in real-time.

## Configuration

The application uses `config.yaml` in the root directory.

```yaml
upstream:
  default:
    type: openai
    api_key: "env:OPENAI_API_KEY" # Reads from environment variable
    base_url: "https://api.openai.com/v1"

logging:
  sink: stdout

analysis:
  enabled: true
  broadcast_channel_size: 1024
```

## Running

1. Set your OpenAI API key:
   ```bash
   export OPENAI_API_KEY=your_key_here
   ```
2. Run the server:
   ```bash
   cargo run
   ```

The server listens on `127.0.0.1:3000`.

## Testing

Run the unit tests to verify the logic:

```bash
cargo test
```

## Usage

### 1. Proxy Chat Completion

Send requests to `http://localhost:3000/v1/chat/completions` just like the OpenAI API.

```bash
curl http://localhost:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer unused" \
  -H "X-Conversation-ID: my-conv-1" \
  -d '{
    "model": "gpt-3.5-turbo",
    "messages": [{"role": "user", "content": "Hello!"}],
    "stream": true
  }'
```

### 2. Inject Context

Inject messages into the next request for a specific conversation ID.

```bash
curl -X POST http://localhost:3000/inject/my-conv-1 \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [
      { "role": "system", "content": "You are a helpful pirate." }
    ]
  }'
```

### 3. Stream Logs

Connect to the SSE endpoint to see live logs.

```bash
curl http://localhost:3000/logs/stream
```
