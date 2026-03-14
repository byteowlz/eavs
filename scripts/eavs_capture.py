#!/usr/bin/env python3
# type: ignore
"""
mitmproxy addon to transparently capture LLM API traffic and route it through Eaves.

This is an OPTIONAL capture mode. The standard approach of pointing clients directly
at Eaves with X-Provider headers remains the primary usage method.

This addon enables zero-config interception of LLM API calls from any application,
including desktop apps like ChatGPT, Claude, and coding assistants.

Usage:
    # Capture all local traffic
    mitmproxy --mode local -s eavs_capture.py

    # Capture specific app only
    mitmproxy --mode local:ChatGPT -s eavs_capture.py

    # With custom Eaves port
    mitmproxy --mode local -s eavs_capture.py --set eavs_port=8080

    # Verbose logging
    mitmproxy --mode local -s eavs_capture.py --set eavs_verbose=true

    # API traffic only (skip desktop app domains)
    mitmproxy --mode local -s eavs_capture.py --set eavs_api_only=true

Requirements:
    - mitmproxy 10.1.5+ (for local capture mode)
    - Eaves proxy running (default: http://127.0.0.1:3033)

On first run, you may need to trust mitmproxy's CA certificate.
See: https://docs.mitmproxy.org/stable/concepts/certificates/
"""

from __future__ import annotations

# Import mitmproxy if available (not needed for standalone testing)
try:
    from mitmproxy import http, ctx

    MITMPROXY_AVAILABLE = True
except ImportError:
    MITMPROXY_AVAILABLE = False
    http = None
    ctx = None

# =============================================================================
# LLM API Domains Configuration
# =============================================================================

# API endpoints for LLM providers (used by SDKs, CLI tools, custom apps)
LLM_API_DOMAINS = {
    # OpenAI
    "api.openai.com",
    # Anthropic
    "api.anthropic.com",
    # Google AI / Vertex AI
    "generativelanguage.googleapis.com",
    "aiplatform.googleapis.com",
    # Mistral
    "api.mistral.ai",
    # Groq
    "api.groq.com",
    # Cerebras
    "api.cerebras.ai",
    # xAI (Grok)
    "api.x.ai",
    # OpenRouter
    "openrouter.ai",
    # Together AI
    "api.together.xyz",
    # Cohere
    "api.cohere.ai",
    "api.cohere.com",
    # Fireworks AI
    "api.fireworks.ai",
    # Perplexity
    "api.perplexity.ai",
    # DeepSeek
    "api.deepseek.com",
    # AI21
    "api.ai21.com",
    # Replicate
    "api.replicate.com",
}

# Desktop app domains (web-based chat interfaces accessed by Electron apps)
# These use different endpoints than the API
DESKTOP_APP_DOMAINS = {
    # ChatGPT Desktop App
    "chat.openai.com",
    "chatgpt.com",
    "cdn.oaistatic.com",  # ChatGPT assets
    "ab.chatgpt.com",  # A/B testing endpoints
    # Claude Desktop App
    "claude.ai",
    "api.claude.ai",
    # Google AI Studio / Gemini
    "aistudio.google.com",
    "gemini.google.com",
    # Perplexity
    "www.perplexity.ai",
    "perplexity.ai",
    # Poe
    "poe.com",
    # Character.AI
    "character.ai",
    "beta.character.ai",
}

# Combine all domains for interception
ALL_LLM_DOMAINS = LLM_API_DOMAINS | DESKTOP_APP_DOMAINS

# Domains to passthrough (logging only, no routing through Eaves)
# These are captured for analytics but don't need provider translation
PASSTHROUGH_DOMAINS = {
    "cdn.oaistatic.com",  # Static assets
}

# =============================================================================
# Provider Detection
# =============================================================================


def detect_provider_from_host(host):
    """
    Map a hostname to an Eaves provider name.

    Returns None if the host should use Eaves' default provider detection,
    or a specific provider name to set in X-Provider header.
    """
    host_lower = host.lower()

    # OpenAI
    if any(d in host_lower for d in ["openai.com", "chatgpt.com"]):
        return "openai"

    # Anthropic
    if "anthropic.com" in host_lower or "claude.ai" in host_lower:
        return "anthropic"

    # Google
    if any(
        d in host_lower for d in ["googleapis.com", "google.com", "gemini.google.com"]
    ):
        return "google"

    # Mistral
    if "mistral.ai" in host_lower:
        return "mistral"

    # Groq
    if "groq.com" in host_lower:
        return "groq"

    # Cerebras
    if "cerebras.ai" in host_lower:
        return "cerebras"

    # xAI
    if "x.ai" in host_lower:
        return "xai"

    # OpenRouter
    if "openrouter.ai" in host_lower:
        return "openrouter"

    # Together
    if "together.xyz" in host_lower:
        return "together"

    # Cohere
    if "cohere.ai" in host_lower or "cohere.com" in host_lower:
        return "cohere"

    # Perplexity
    if "perplexity.ai" in host_lower:
        return "perplexity"

    # DeepSeek
    if "deepseek.com" in host_lower:
        return "deepseek"

    return None


def is_llm_domain(host):
    """Check if a host is an LLM-related domain."""
    host_lower = host.lower()
    return any(domain in host_lower for domain in ALL_LLM_DOMAINS)


def is_passthrough_domain(host):
    """Check if domain should be logged but not routed through Eaves."""
    host_lower = host.lower()
    return any(domain in host_lower for domain in PASSTHROUGH_DOMAINS)


def is_api_domain(host):
    """Check if a host is an LLM API domain (not desktop app)."""
    host_lower = host.lower()
    return any(domain in host_lower for domain in LLM_API_DOMAINS)


# =============================================================================
# mitmproxy Addon
# =============================================================================


class EavsCapture:
    """
    mitmproxy addon that intercepts LLM API traffic and routes it through Eaves.

    Features:
    - Transparent interception of all LLM API calls
    - Desktop app traffic capture (ChatGPT, Claude, etc.)
    - Automatic provider detection
    - Preserves original request for Eaves to handle authentication
    - Configurable Eaves endpoint
    """

    def load(self, loader):
        """Register addon options."""
        loader.add_option(
            name="eavs_host",
            typespec=str,
            default="127.0.0.1",
            help="Eaves proxy host",
        )
        loader.add_option(
            name="eavs_port",
            typespec=int,
            default=3033,
            help="Eaves proxy port",
        )
        loader.add_option(
            name="eavs_verbose",
            typespec=bool,
            default=False,
            help="Enable verbose logging of intercepted requests",
        )
        loader.add_option(
            name="eavs_capture_desktop",
            typespec=bool,
            default=True,
            help="Capture desktop app traffic (ChatGPT, Claude, etc.)",
        )
        loader.add_option(
            name="eavs_passthrough_mode",
            typespec=bool,
            default=False,
            help="Log traffic only, don't route through Eaves (for debugging)",
        )
        loader.add_option(
            name="eavs_api_only",
            typespec=bool,
            default=False,
            help="Only capture API traffic, skip desktop app domains",
        )

    def running(self):
        """Called when mitmproxy is fully running."""
        ctx.log.info(
            f"[EAVS] Capture addon loaded. "
            f"Routing LLM traffic to {ctx.options.eavs_host}:{ctx.options.eavs_port}"
        )
        if ctx.options.eavs_capture_desktop and not ctx.options.eavs_api_only:
            ctx.log.info("[EAVS] Desktop app capture enabled (ChatGPT, Claude, etc.)")
        if ctx.options.eavs_api_only:
            ctx.log.info("[EAVS] API-only mode: skipping desktop app domains")
        if ctx.options.eavs_passthrough_mode:
            ctx.log.warn(
                "[EAVS] Passthrough mode enabled - traffic will NOT be routed through Eaves"
            )

    def request(self, flow):
        """Process each request and route LLM traffic through Eaves."""
        host = flow.request.pretty_host

        # Check if this is LLM-related traffic
        if not is_llm_domain(host):
            return  # Not LLM traffic, let it pass through

        # API-only mode skips desktop app domains
        if ctx.options.eavs_api_only and not is_api_domain(host):
            if ctx.options.eavs_verbose:
                ctx.log.info(f"[EAVS] Skipping desktop domain (api-only mode): {host}")
            return

        # Check if desktop app capture is disabled
        if not ctx.options.eavs_capture_desktop:
            if host in DESKTOP_APP_DOMAINS:
                return

        # Log the interception
        if ctx.options.eavs_verbose:
            ctx.log.info(
                f"[EAVS] Intercepted: {flow.request.method} {host}{flow.request.path}"
            )

        # Passthrough mode - just log, don't redirect
        if ctx.options.eavs_passthrough_mode:
            flow.request.headers["X-Eavs-Captured"] = "true"
            return

        # Skip passthrough domains (logged but not routed)
        if is_passthrough_domain(host):
            if ctx.options.eavs_verbose:
                ctx.log.info(f"[EAVS] Passthrough (no routing): {host}")
            return

        # Detect provider
        provider = detect_provider_from_host(host)

        # Store original request info for Eaves
        flow.request.headers["X-Original-Host"] = host
        flow.request.headers["X-Original-Scheme"] = flow.request.scheme
        flow.request.headers["X-Original-Port"] = str(flow.request.port)

        if provider:
            flow.request.headers["X-Provider"] = provider

        # Redirect to Eaves
        flow.request.host = ctx.options.eavs_host
        flow.request.port = ctx.options.eavs_port
        flow.request.scheme = "http"

        ctx.log.info(
            f"[EAVS] {flow.request.method} {host}{flow.request.path} "
            f"-> {provider or 'auto-detect'}"
        )

    def response(self, flow):
        """Log responses for debugging."""
        if not ctx.options.eavs_verbose:
            return

        # Only log if we intercepted this request
        if "X-Original-Host" not in flow.request.headers:
            return

        original_host = flow.request.headers.get("X-Original-Host", "unknown")
        ctx.log.info(
            f"[EAVS] Response: {flow.response.status_code} "
            f"from {original_host}{flow.request.path}"
        )

    def error(self, flow):
        """Log errors for debugging."""
        if "X-Original-Host" not in flow.request.headers:
            return

        original_host = flow.request.headers.get("X-Original-Host", "unknown")
        ctx.log.error(
            f"[EAVS] Error for {original_host}{flow.request.path}: {flow.error}"
        )


# Register the addon (only when mitmproxy is available)
if MITMPROXY_AVAILABLE:
    addons = [EavsCapture()]


# =============================================================================
# Standalone testing (run with: python eavs_capture.py)
# =============================================================================

if __name__ == "__main__":
    # Test provider detection
    test_hosts = [
        "api.openai.com",
        "chat.openai.com",
        "api.anthropic.com",
        "claude.ai",
        "generativelanguage.googleapis.com",
        "api.groq.com",
        "api.mistral.ai",
        "api.cerebras.ai",
        "api.x.ai",
        "openrouter.ai",
        "api.together.xyz",
        "api.perplexity.ai",
        "api.deepseek.com",
        "cdn.oaistatic.com",
        "unknown.example.com",
    ]

    print("Eaves Capture - Provider Detection Test")
    print("=" * 70)
    print(f"{'Host':<45} {'Provider':<15} {'LLM?':<6} {'API?'}")
    print("-" * 70)
    for host in test_hosts:
        provider = detect_provider_from_host(host)
        is_llm = is_llm_domain(host)
        is_api = is_api_domain(host)
        print(f"{host:<45} {provider or 'None':<15} {str(is_llm):<6} {is_api}")
