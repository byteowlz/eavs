import type { Scheme, IdentityTokens } from '@byteowlz/design-system';
export type ResourceType = 'machine' | 'workspace' | 'harness' | 'provider';
interface ResourceBase { id:string; name:string; detail?:string; }
export type Resource = ResourceBase & ({ type:'provider'; models:readonly string[] } | { type:'machine'|'workspace'|'harness'; models?:readonly string[] });
export type DeepReadonly<T> = T extends object ? { readonly [K in keyof T]:DeepReadonly<T[K]> } : T;
export interface ProviderDeclaration { id:string; source:'stored'|'toml'; models:readonly string[]; }
export interface Instance { id:string; name:string; revision:string; available:boolean; providers:readonly ProviderDeclaration[]; }
export interface Tenant { id:string; name:string; resources:readonly Resource[]; links:readonly {from:string;to:string}[]; instances:readonly Instance[]; }
export interface Inventory { version:string; tenants:readonly Tenant[]; }
export interface Binding { from:string; provider:string; model:string; }
export interface Plan { kind:'policy-intent'; inventoryVersion:string; tenantId:string; targets:readonly {id:string;expectedRevision:string}[]; bindings:readonly Binding[]; }
export type PreviewStatus = 'inventory-ready'|'unavailable'|'revision-conflict'|'provider-missing'|'model-missing';
export type SimulationStatus = 'simulated-applied'|'simulated-conflict'|'simulated-skipped';
export interface Result<K extends 'preview'|'simulation',S> { kind:K; inventoryVersion:string; tenantId:string; plan:Plan; rows:readonly {id:string;status:S}[]; }
export interface ReadOnlyClient { mode:'read-only'; preview(plan:Plan, options?:{signal?:AbortSignal}):Promise<Result<'preview',PreviewStatus>>; }
export interface SimulationClient { mode:'simulation'; preview(plan:Plan, options?:{signal?:AbortSignal}):Promise<Result<'preview',PreviewStatus>>; simulate(plan:Plan, options?:{signal?:AbortSignal}):Promise<Result<'simulation',SimulationStatus>>; }
export type AdminClient = ReadOnlyClient | SimulationClient;
export type Theme = 'inherit' | { scheme:Scheme; identity?:Partial<IdentityTokens>; radius?:string; };
export interface IntentSummary { tenantId:string; inventoryVersion:string; targetIds:readonly string[]; bindings:readonly Binding[]; }
export interface Options { inventory:Inventory; client?:AdminClient; tenantId?:string; theme?:Theme; onIntent?:(intent:IntentSummary)=>void; }
export interface ControlRoomModule { update(options:Partial<Options>):void; setTheme(theme:Theme):void; destroy():void; }
export function mountControlRoom(root:HTMLElement, options:Options):ControlRoomModule;
export function normalizeInventory(input:Inventory):DeepReadonly<Inventory>;
export function createReadOnlyClient(input:Inventory):ReadOnlyClient;
export function createSimulationClient(input:Inventory):SimulationClient;
export const controlRoomScheme:Scheme;
export const controlRoomIdentity:Partial<IdentityTokens>;
export interface OqtoSnapshot { version:string; tenants:readonly { id:string; name:string; instances:readonly Instance[]; machines:readonly {id:string;name:string}[]; workspaces:readonly {id:string;name:string;machineId?:string}[]; harnesses:readonly {id:string;name:string;workspaceId?:string}[]; providers:readonly {id:string;name:string;models:readonly string[]}[]; }[]; }
export function projectOqtoInventory(snapshot:OqtoSnapshot):Inventory;
export function mountOqtoControlRoom(root:HTMLElement, options:{snapshot:OqtoSnapshot;client?:AdminClient;tenantId?:string;onIntent?:Options['onIntent']}):{ update(options:{snapshot?:OqtoSnapshot;client?:AdminClient;tenantId?:string;onIntent?:Options['onIntent']}):void;destroy():void; };
