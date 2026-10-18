import type {DeviceConnection, NativeProvider} from "./provider-library.ts";
import {fetchPiProviders} from "./provider-library.ts";

export interface ProviderRegistry {
 registerProvider(name: string, definition: NativeProvider): void;
 unregisterProvider(name: string): void;
}
// Reusable by an extension or an embedding harness. Only its own registrations
// are replaced; errors retain last-known metadata, not authorization or routing.
export class LiveProviders {
 private owned = new Map<string,NativeProvider>();
 private generation = 0;
 private pending?: AbortController;
 private closed = false;
 private registry: ProviderRegistry;
 constructor(registry: ProviderRegistry) {this.registry=registry;}
 async refresh(connection: DeviceConnection, env: NodeJS.ProcessEnv = process.env): Promise<number|undefined> {
  if (this.closed) return;
  this.pending?.abort();const controller = new AbortController();this.pending = controller;
  const generation = ++this.generation;
  let projection: Awaited<ReturnType<typeof fetchPiProviders>>;
  try {projection = await fetchPiProviders(connection,env,controller.signal);} catch {
   if (generation !== this.generation || this.closed) return;
   throw new Error("eavs catalog unavailable; no inference was attempted");
  }
  if (generation !== this.generation || this.closed) return;
  const next = new Map(Object.entries(projection.providers));
  const installed: string[] = [];
  try {
   for (const [name,definition] of next) {this.registry.registerProvider(name,definition);installed.push(name);}
   for (const name of this.owned.keys()) if (!next.has(name)) this.registry.unregisterProvider(name);
  } catch {
   for (const name of installed) if (!this.owned.has(name)) {try {this.registry.unregisterProvider(name);} catch {/* Continue bounded best-effort rollback; never expose registry errors. */}}
   for (const [name,definition] of this.owned) {try {this.registry.registerProvider(name,definition);} catch {/* Gateway remains authoritative; failed registries are not a transaction. */}}
   throw new Error("eavs provider registration unavailable; no inference was attempted");
  }
  this.owned = next;
  return projection.skipped_models;
 }
 close(): void {
  if (this.closed) return;
  this.closed = true;this.generation++;this.pending?.abort();
  for (const name of this.owned.keys()) {try {this.registry.unregisterProvider(name);} catch {/* Release all remaining owned entries on shutdown. */}}
  this.owned.clear();
 }
}
