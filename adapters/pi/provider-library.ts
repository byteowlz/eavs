// Credential-blind Pi projection and explicit trusted device-profile transport.
// No subprocesses, credential commands, secret-file references, writes or inference.
import { constants } from "node:fs";
import { open, lstat } from "node:fs/promises";
import { request as httpRequest, Agent as HttpAgent } from "node:http";
import { request as httpsRequest, Agent as HttpsAgent } from "node:https";
import { Buffer } from "node:buffer";

const LIMIT = 4 * 1024 * 1024;
const identifier = (v: unknown): v is string => typeof v === "string" && /^[A-Za-z0-9_.-]{1,128}$/.test(v) && v !== "." && v !== "..";
const reference = (v: unknown): v is string => typeof v === "string" && /^[A-Za-z_][A-Za-z0-9_]{0,127}$/.test(v);
const text = (v: unknown): v is string => typeof v === "string" && v.length > 0 && Buffer.byteLength(v) <= 512 && !/[\u0000-\u001f\u007f-\u009f]/.test(v);
const object = (v: unknown): v is Record<string, any> => !!v && typeof v === "object" && !Array.isArray(v);
function invalid(): never { throw new Error("eavs device metadata unavailable or invalid"); }
export interface DeviceConnection { name: string; url: string; key_env: string; listener_key_env?: string; allow_loopback_http: boolean }
export interface NativeModel {id: string; name: string; reasoning: boolean; input: ("text"|"image")[]; contextWindow: number; maxTokens: number; cost: {input: number; output: number; cacheRead: number; cacheWrite: number}; compat: Record<string, any>}
export interface NativeProvider {baseUrl: string; api: "openai-completions"|"openai-responses"|"anthropic-messages"; apiKey: string; headers: Record<string,string>; models: NativeModel[]; compat?: Record<string,any>}
function origin(connection: DeviceConnection): URL {
  let url: URL;
  try {url = new URL(connection.url);} catch {return invalid();}
  const local = ["127.0.0.1", "[::1]", "::1"].includes(url.hostname);
  if (!identifier(connection.name) || !reference(connection.key_env) || connection.listener_key_env !== undefined && !reference(connection.listener_key_env) || typeof connection.allow_loopback_http !== "boolean" || url.username || url.password || url.search || url.hash || url.pathname !== "/" || !(url.protocol === "https:" || connection.allow_loopback_http && local && url.protocol === "http:")) return invalid();
  return url;
}
export async function readDeviceConnection(path: string, name: string): Promise<DeviceConnection> {
  if (!identifier(name)) return invalid();
  try {
    const meta = await lstat(path);
    if (!meta.isFile() || meta.isSymbolicLink() || meta.size > LIMIT) return invalid();
    const file = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
    let value: any;
    try {
      const stat = await file.stat();
      if (!stat.isFile() || stat.size > LIMIT || stat.dev !== meta.dev || stat.ino !== meta.ino) return invalid();
      // Read at most LIMIT+1 even if another process grows the file.
      const buffer = Buffer.alloc(LIMIT + 1); let size = 0;
      while (size <= LIMIT) {const result = await file.read(buffer, size, buffer.length - size, null); if (!result.bytesRead) break; size += result.bytesRead;}
      if (size > LIMIT) return invalid();
      value = JSON.parse(new TextDecoder("utf-8",{fatal:true}).decode(buffer.subarray(0,size)));
    } finally {await file.close();}
    if (!object(value) || Object.keys(value).some(k => k !== "connections") || !Array.isArray(value.connections) || value.connections.length > 32) return invalid();
    const names = new Set<string>();
    for (const c of value.connections) {
      if (!object(c) || Object.keys(c).some(k => !["name","url","key_env","listener_key_env","allow_loopback_http"].includes(k))) return invalid();
      origin(c as DeviceConnection);
      if (names.has(c.name)) return invalid(); names.add(c.name);
    }
    const selected = value.connections.find((c: DeviceConnection) => c.name === name);
    if (!selected) return invalid();
    return selected;
  } catch {return invalid();}
}
function compat(value: unknown): Record<string,any> {
  const out: Record<string,any> = {};
  if (!object(value)) return out;
  for (const key of ["supportsStore","supportsDeveloperRole","supportsReasoningEffort","supportsUsageInStreaming","supportsToolChoice","supportsStrictMode","supportsImagesInToolResults"]) if (typeof value[key] === "boolean") out[key] = value[key];
  if (["max_tokens","max_completion_tokens"].includes(value.maxTokensField)) out.maxTokensField = value.maxTokensField;
  return out;
}
function model(value: Record<string,any>): NativeModel | undefined {
  const price = value.pricing;
  if (!object(price) || price.status !== "unverified_estimate" || price.unit !== "USD_per_million_tokens" || !object(price.rates)) return;
  const cost: any = {};
  for (const [from,to] of [["input","input"],["output","output"],["cache_read","cacheRead"],["cache_write","cacheWrite"]]) {
    const rate = price.rates[from];
    if (typeof rate !== "number" || !Number.isFinite(rate) || rate < 0 || ["input","output"].includes(from) && rate <= 0) return;
    cost[to] = rate;
  }
  if (!Number.isInteger(value.context_window) || value.context_window <= 0 || value.context_window > 10_000_000 || !Number.isInteger(value.max_tokens) || value.max_tokens <= 0 || value.max_tokens > value.context_window || typeof value.reasoning !== "boolean" || !Array.isArray(value.input) || !value.input.length || value.input.some((v:unknown) => v !== "text" && v !== "image")) return;
  return {id:value.id,name:value.name,reasoning:value.reasoning,input:value.input,contextWindow:value.context_window,maxTokens:value.max_tokens,cost,compat:compat(value.compat)};
}
export function projectPiProviders(connection: DeviceConnection, catalog: unknown): {providers: Record<string,NativeProvider>; skipped_models: number} {
  origin(connection);
  if (!object(catalog) || catalog.schema_version !== 1 || catalog.scope !== "key-filtered-chat-candidates" || !Array.isArray(catalog.providers) || catalog.providers.length > 256) return invalid();
  const providers: Record<string,NativeProvider> = {}; const seen = new Set(); let count = 0, skipped = 0;
  for (const p of catalog.providers) {
    if (!object(p) || !identifier(p.id) || seen.has(p.id) || !Array.isArray(p.models)) return invalid(); seen.add(p.id);
    const wire: Record<string,[NativeProvider["api"],string]> = {"openai-completions":["openai-completions","/chat/completions"],"openai-responses":["openai-responses","/responses"],"openai-codex-responses":["openai-responses","/responses"],"anthropic-messages":["anthropic-messages","/messages"]};
    const route = typeof p.api === "string" && Object.hasOwn(wire,p.api) ? wire[p.api] : undefined;
    if (!route || p.operation !== "chat" || p.base_path !== `/${p.id}/v1` || p.request_path !== `/${p.id}/v1${route[1]}`) return invalid();
    const models: NativeModel[] = []; const ids = new Set();
    for (const m of p.models) {
      if (++count > 4096 || !object(m) || !text(m.id) || !text(m.name) || ids.has(m.id)) return invalid(); ids.add(m.id);
      const projected = model(m); if (projected) models.push(projected); else skipped++;
    }
    if (!models.length) continue;
    const name = `eavs-device-${Buffer.from(connection.name).toString("base64url")}.${Buffer.from(p.id).toString("base64url")}`;
    const headers: Record<string,string> = {Authorization:`Bearer \${${connection.key_env}}`};
    if (connection.listener_key_env) headers["X-Eavs-Token"] = `\${${connection.listener_key_env}}`;
    providers[name] = {api:route[0],baseUrl:`${connection.url.replace(/\/$/, "")}${p.base_path}`,apiKey:`\${${connection.key_env}}`,headers,models};
    if (route[0] === "openai-completions") providers[name].compat = compat(p.compat);
  }
  if (!Object.keys(providers).length && skipped) return invalid();
  return {providers,skipped_models:skipped};
}
async function bunCatalog(url: URL,headers:Record<string,string>,signal?:AbortSignal):Promise<Buffer> {
  const controller=new AbortController();
  const abort=()=>controller.abort();const timer=setTimeout(abort,6_000);
  signal?.addEventListener("abort",abort,{once:true});
  let reader:ReadableStreamDefaultReader<Uint8Array>|undefined;
  try {
    if (signal?.aborted) abort();
    // Bun's node:http shim still consults proxy env even with agent:false.
    // New Bun supports proxy:false, but tested Bun1.3.13 ignores all bypass
    // values when proxy env is configured. Fail closed instead of mutating the
    // process environment shared with other extensions or leaking a virtual key.
    if (["HTTP_PROXY","http_proxy","HTTPS_PROXY","https_proxy","ALL_PROXY","all_proxy"].some(name=>process.env[name])) throw new Error("Bun proxy bypass unavailable");
    const options:RequestInit & {proxy:false;tls:{rejectUnauthorized:true}}={method:"GET",headers,redirect:"manual",signal:controller.signal,proxy:false,tls:{rejectUnauthorized:true}};
    const response=await fetch(url,options);
    reader=response.body?.getReader();
    if(response.status!==200 || !reader) throw new Error("catalog unavailable");
    const chunks:Buffer[]=[];let size=0;
    for (;;) {const chunk=await reader.read();if(chunk.done)break;size+=chunk.value.byteLength;if(size>LIMIT)throw new Error("catalog bound");chunks.push(Buffer.from(chunk.value));}
    return Buffer.concat(chunks);
  } catch {throw new Error("eavs catalog unavailable; no inference was attempted");}
  finally {clearTimeout(timer);signal?.removeEventListener("abort",abort);if(reader){try{await reader.cancel();}catch{/* Already aborted/closed. */}reader.releaseLock();}}
}
export async function fetchPiProviders(connection: DeviceConnection, env: NodeJS.ProcessEnv = process.env, signal?: AbortSignal) {
  const base = origin(connection); const key = env[connection.key_env];
  const listener = connection.listener_key_env ? env[connection.listener_key_env] : undefined;
  if (!key || !/^eavs-[A-Za-z0-9_-]+$/.test(key) || connection.listener_key_env && (!listener || /[^\t\x20-\x7e\x80-\xff]/.test(listener))) return invalid();
  const headers:Record<string,string>={Authorization:`Bearer ${key}`};if(listener)headers["X-Eavs-Token"]=listener;
  const body = (globalThis as {Bun?:unknown}).Bun ? await bunCatalog(new URL("/v1/eavs/catalog",base),headers,signal) : await new Promise<Buffer>((resolve,reject) => {
    const chunks: Buffer[] = []; let size = 0; let settled = false;
    const fail = () => {if (!settled) {settled=true; reject(new Error("eavs catalog unavailable; no inference was attempted"));}};
    let request: ReturnType<typeof httpRequest> | undefined;
    const agentOptions={keepAlive:false,proxyEnv:{},rejectUnauthorized:true};
    const agent=base.protocol==="https:" ? new HttpsAgent(agentOptions) : new HttpAgent(agentOptions);
    const abort = () => {fail(); request?.destroy();};
    const timer = setTimeout(abort, 6_000);
    try {
      // Explicit standard-library transport: no redirects, environment proxies,
      // cookies, credentials-file reads, retry, probe or generating request.
      request = (base.protocol === "https:" ? httpsRequest : httpRequest)(new URL("/v1/eavs/catalog",base), {method:"GET",agent,headers,maxHeaderSize:16*1024,rejectUnauthorized:true}, response => {
        if (response.statusCode !== 200) {fail(); response.destroy(); return;}
        response.on("error",fail);
        response.once("close",() => {if (!response.complete) fail();});
        response.on("data",chunk => {size+=chunk.length; if (size>LIMIT) {fail(); request?.destroy();} else chunks.push(Buffer.from(chunk));});
        response.on("end",() => {if (!settled) {settled=true; resolve(Buffer.concat(chunks));}});
      });
      request.on("error",fail);
      signal?.addEventListener("abort",abort,{once:true});
      request.once("close",() => {agent.destroy();clearTimeout(timer);signal?.removeEventListener("abort",abort);});
      if (signal?.aborted) abort(); else request.end();
    } catch {agent.destroy();clearTimeout(timer);signal?.removeEventListener("abort",abort);abort();}
  });
  try {return projectPiProviders(connection,JSON.parse(new TextDecoder("utf-8",{fatal:true}).decode(body)));} catch {return invalid();}
}
