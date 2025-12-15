Below is a concise PRD capturing exactly the system you described:
a local, Rust-based LLM proxy with zero-latency bidirectional streaming, full logging, and live context injection.

⸻

Product Requirements Document (PRD)

Local Bidirectional LLM Proxy with Live Analysis & Context Injection

⸻

1. Purpose

Build a lightweight Rust proxy that sits between an LLM client and any upstream model provider, forwarding all traffic with no added latency while enabling:
	•	Full logging of all requests and streamed responses
	•	Live, out-of-band analysis of interactions
	•	Bidirectional context injection (pre-request or mid-stream when supported)

This enables real-time modification, auditing, and augmentation of LLM conversations without changing client code.

⸻

2. Core Capabilities

2.1 Transparent Forward Proxy
	•	Accepts OpenAI-compatible API requests (/v1/chat/completions, etc.)
	•	Forwards requests to configured upstream providers
	•	Maintains streaming transparency (SSE / chunked / WS)
	•	Adds no measurable additional latency beyond network forwarding

⸻

2.2 Comprehensive Interaction Logging
	•	Log all inbound requests:
	•	URL, method, headers
	•	Raw JSON body
	•	Timestamp + correlation ID
	•	Log all streamed responses:
	•	Token chunks or event frames
	•	Timestamp + correlation ID
	•	Logging destinations (configurable):
	•	File, stdout, or UNIX socket
	•	Pluggable sinks (Kafka, NATS, Redis)

⸻

2.3 Live Analysis Pipeline
	•	The proxy exposes an async broadcast channel that delivers all logs to an external “analysis module”
	•	Analyzer can:
	•	Observe every token
	•	Track conversation state
	•	Compute new context to inject
	•	Analyzer communicates back via a simple HTTP/WebSocket control channel:
	•	POST /inject/{conversation_id}
	•	Payload contains system/user/assistant messages to inject

⸻

2.4 Context Injection

Two modes:

2.4.1 Pre-Request Injection (default)
Before forwarding a request upstream:
	•	Look up queued injections for this conversation
	•	Modify the request JSON:
	•	Insert system messages
	•	Append assistant messages
	•	Rewrite metadata

This happens synchronously and adds no extra latency.

2.4.2 Mid-Stream Injection (optional via WS)
When using OpenAI Realtime or other WS protocols:
	•	Proxy can send synthesized events/messages upstream or downstream mid-stream
	•	Enables tool-call injection, instruction overrides, or structured context updates

⸻

3. Architecture Overview

3.1 Components
	1.	Proxy Server (Rust, Axum/Hyper)
	•	Forwards traffic
	•	Streams transparently
	•	Logs everything
	•	Hosts control API
	2.	Analyzer Module (external)
	•	Consumes logs
	•	Performs NLP or logic
	•	Pushes context back via /inject
	3.	Context Store
	•	In-memory hashmap keyed by conversation ID
	•	Stored injections are merged on next request
	4.	Provider Layer
	•	Config file listing model endpoints & API keys

⸻

4. API Surface

4.1 Forwarded API
	•	/v1/chat/completions
	•	/v1/completions
	•	/v1/audio/*
	•	/v1/responses (optional)

Same semantics and schema as OpenAI’s API.

⸻

4.2 Control API

For analyzer integration:

POST /inject/{conversation_id}

{
  "messages": [
    { "role": "system", "content": "..." }
  ]
}

POST /clear/{conversation_id}
Clears pending injections.

GET  /logs/stream
Optional real-time event stream of logs.

⸻

5. Performance Requirements
	•	Added latency: < 1ms per chunk
	•	Maximum throughput: 10k concurrent streams
	•	Memory overhead: < 20MB idle, < 200MB under load
	•	CPU footprint: minimal; passes through bytes without parse unless injection required

⸻

6. Security Requirements
	•	Runs entirely locally (no exfiltration)
	•	Logs can be disabled or encrypted
	•	API keys stored only in local config file
	•	Analyzer has no access to raw provider secrets

⸻

7. Configuration

Single YAML file:

upstream:
  default:
    type: openai
    api_key: env:OPENAI_API_KEY
    base_url: https://api.openai.com/v1

logging:
  sink: stdout

analysis:
  enabled: true
  broadcast_channel_size: 1024


⸻

8. Non-Goals
	•	Not a multi-tenant API gateway
	•	No built-in billing or provider pooling
	•	Not an orchestration layer like LiteLLM

⸻

9. Milestones

MVP (2–3 days)
	•	Transparent OpenAI-compatible forward proxy
	•	Logging (requests + streamed responses)
	•	Pre-request injection via /inject endpoint
	•	Basic YAML config

v1.0
	•	WebSocket support for mid-stream injection
	•	Multiple upstream model configs
	•	Pluggable logging backends
	•	Temp conversation state store & TTL

v1.1
	•	Plugin system for analyzer integration
	•	Fine-grained policy engine (rewrite, filter, deny rules)

⸻

Want the code scaffold next?

If you want, I can generate a starter Rust project with:
	•	Axum server
	•	Streaming-forwarding proxy
	•	Log teeing
	•	Injection middleware
	•	Config parsing
	•	Conversation state store

Just tell me: “create the starter Rust project”, and I’ll output it.
