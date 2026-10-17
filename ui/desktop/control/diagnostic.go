package control

// DiagnosticLabel displays closed, non-secret codes, never raw transport errors.
func DiagnosticLabel(code string) string {
	switch code {
	case "":
		return ""
	case "listener_credential_missing_or_invalid":
		return "Configure the separate device-local listener capability reference."
	case "credential_missing":
		return "Configure the device-local credential environment reference."
	case "scoped_key_required":
		return "A scoped virtual key is required; master credentials cannot substitute."
	case "authentication_rejected":
		return "The gateway rejected this device's authentication."
	case "rate_limited":
		return "The metadata request was rate limited; no fallback was attempted."
	case "server_failure":
		return "The gateway reported a server failure."
	case "unexpected_http_status":
		return "The endpoint returned an unsupported HTTP status."
	case "transport_failure":
		return "The gateway could not be reached; this is not proof that Tailscale is disconnected."
	case "catalog_limit":
		return "The catalog exceeded the supported size bound."
	case "invalid_catalog":
		return "The catalog did not match the supported schema."
	case "catalog_available":
		return "Current scoped candidate metadata is available."
	case "transport_timeout":
		return "The metadata request timed out."
	default:
		return "Unrecognized diagnostic; inspect connection configuration."
	}
}
