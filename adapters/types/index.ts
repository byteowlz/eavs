/**
 * @eavs/adapter-types - TypeScript types for eavs model export adapters
 *
 * Each adapter transforms eavs provider/model data into the config format
 * expected by a specific agent harness (Pi, OpenCode, etc.).
 *
 * Adapters receive a JSON request on stdin and write the result to stdout.
 */

/** Cost per million tokens */
export interface ModelCost {
  input: number;
  output: number;
  cache_read: number;
  cache_write?: number;
}

/** A model entry from the eavs catalog or config shortlist */
export interface EavsModel {
  id: string;
  name: string;
  reasoning: boolean;
  input: string[];
  context_window: number;
  max_tokens: number;
  cost: ModelCost;
  /** Per-model compat flags, already in the target harness's schema */
  compat?: Record<string, unknown>;
}

/** Provider-level compat flags from eavs config / URL detection */
export interface EavsCompat {
  supports_store?: boolean;
  supports_developer_role?: boolean;
  max_tokens_field?: string;
  supports_stream_options?: boolean;
}

/** A configured provider from eavs */
export interface EavsProvider {
  /** Provider name as configured (e.g., "openai", "anthropic") */
  name: string;
  /** Provider type (e.g., "openai", "anthropic", "openai-codex") */
  type: string;
  /** Pi-compatible API type (e.g., "openai-responses", "anthropic-messages") */
  pi_api: string | null;
  /** Whether this provider uses OAuth */
  oauth: boolean;
  /** Whether the provider has a resolved API key */
  has_api_key: boolean;
  /** Provider-level compat flags (explicit config + URL-detected defaults) */
  compat?: EavsCompat;
  /** Model list: config shortlist if set, otherwise full catalog */
  models: EavsModel[];
}

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

/** Full export -- produce a complete config from scratch */
export interface ExportRequest {
  method: "export";
  providers: EavsProvider[];
  base_url: string;
  api_key: string;
}

/**
 * Merge -- add/update eavs providers into an existing config.
 *
 * The adapter receives the current config file content. It must:
 *   - Replace/add all eavs-managed entries
 *   - Leave non-eavs entries untouched
 *   - Return the full merged config
 */
export interface MergeRequest {
  method: "merge";
  providers: EavsProvider[];
  base_url: string;
  api_key: string;
  /** Current config file content (JSON string) */
  existing: string;
}

/** Info request -- adapter returns metadata about itself */
export interface InfoRequest {
  method: "info";
}

export type AdapterRequest = ExportRequest | MergeRequest | InfoRequest;

/** Adapter metadata */
export interface AdapterInfo {
  /** Adapter name matching the directory (e.g., "pi") */
  name: string;
  /** Human-readable name (e.g., "Pi Coding Agent") */
  displayName: string;
  /** Adapter version */
  version: string;
  /** Output filename suggestion (e.g., "models.json") */
  outputFile: string;
  /** Default output path (e.g., "~/.pi/agent/models.json") */
  defaultPath: string;
  /** Description of what this adapter produces */
  description: string;
  /** Prefix used to identify eavs-managed entries (e.g., "eavs-") */
  managedPrefix: string;
}

/** Response from the adapter -- either info or the exported JSON */
export type AdapterResponse = AdapterInfo | Record<string, unknown>;

// ---------------------------------------------------------------------------
// Adapter runner
// ---------------------------------------------------------------------------

/** Helper: read full stdin as string */
export async function readStdin(): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) {
    chunks.push(Buffer.from(chunk));
  }
  return Buffer.concat(chunks).toString("utf-8");
}

/** Standard adapter runner -- handles request routing */
export function runAdapter(adapter: {
  info(): AdapterInfo;
  export(req: ExportRequest): Record<string, unknown>;
  merge(req: MergeRequest): Record<string, unknown>;
}): void {
  (async () => {
    try {
      const input = await readStdin();
      if (!input.trim()) {
        console.error(JSON.stringify({ error: "No input provided on stdin" }));
        process.exit(1);
      }

      const request: AdapterRequest = JSON.parse(input);

      let response: AdapterResponse;
      switch (request.method) {
        case "info":
          response = adapter.info();
          break;
        case "export":
          response = adapter.export(request);
          break;
        case "merge":
          response = adapter.merge(request);
          break;
        default:
          console.error(
            JSON.stringify({
              error: `Unknown method: ${(request as { method: string }).method}`,
            })
          );
          process.exit(1);
      }

      console.log(JSON.stringify(response, null, 2));
    } catch (err) {
      console.error(
        JSON.stringify({
          error: err instanceof Error ? err.message : String(err),
        })
      );
      process.exit(1);
    }
  })();
}
