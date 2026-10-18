import { test, expect } from 'bun:test';
import { normalizeInventory, createDraft, addBinding, createPlan, validateResult } from '../src/model.js';
import { createReadOnlyClient, createSimulationClient, projectOqtoInventory } from '../dist/control-room.js';
import { sampleInventory, sampleOqtoSnapshot } from '../demo/inventory.js';

function setup(provider='openai',model='gpt-5.4-mini') {
  const inventory=normalizeInventory(sampleInventory),tenant=inventory.tenants[0],draft=createDraft(tenant);
  draft.targets.add('east');draft.targets.add('west');addBinding(tenant,draft,'pi',provider,model);
  return {inventory,tenant,draft,plan:createPlan(inventory,tenant.id,draft)};
}
test('allowlist, clone and deeply freeze inventory; no unknown credential fields',()=>{
  const input=structuredClone(sampleInventory);input.apiKey='SYNTHETIC-SECRET';input.tenants[0].instances[0].token='SYNTHETIC-SECRET';input.tenants[0].resources[0].credentialCommand='DO-NOT-EXECUTE';
  const value=normalizeInventory(input);input.tenants[0].resources[0].name='changed';
  expect(value.tenants[0].resources[0].name).toBe('Atelier Mac');expect(JSON.stringify(value)).not.toContain('SYNTHETIC-SECRET');expect(JSON.stringify(value)).not.toContain('DO-NOT-EXECUTE');expect(Object.isFrozen(value.tenants[0].instances[0].providers)).toBe(true);
});
test('no implicit targets, unknown models, provider-to-provider or foreign resources',()=>{
  const {inventory,tenant}=setup(),draft=createDraft(tenant);
  expect(draft.targets.size).toBe(0);expect(()=>createPlan(inventory,'studio',draft)).toThrow();
  expect(()=>addBinding(tenant,draft,'pi','openai','unlisted-alias')).toThrow();expect(()=>addBinding(tenant,draft,'anthropic','openai','gpt-5.4-mini')).toThrow();expect(()=>addBinding(tenant,draft,'foreign-harness','openai','gpt-5.4-mini')).toThrow();
});
test('duplicate bindings no-op; typed machine, workspace and harness source intents',()=>{
  const {tenant,draft}=setup();expect(addBinding(tenant,draft,'pi','openai','gpt-5.4-mini')).toBe(false);expect(addBinding(tenant,draft,'mac','openai','gpt-5.4-mini')).toBe(true);expect(addBinding(tenant,draft,'govnr','anthropic','claude-haiku-5-5')).toBe(true);expect(draft.bindings.length).toBe(3);
});
test('foreign and unavailable target selection fails closed',()=>{
  const {inventory,draft}=setup();draft.targets.add('travel');expect(()=>createPlan(inventory,'studio',draft)).toThrow();draft.targets.delete('travel');draft.targets.add('sandbox');expect(()=>createPlan(inventory,'studio',draft)).toThrow();
});
test('opaque identifiers cannot access inherited position properties',()=>{
  const draft=createDraft(normalizeInventory(sampleInventory).tenants[0]);expect(draft.positions['__proto__']).toBeUndefined();expect(draft.positions['constructor']).toBeUndefined();draft.positions['__proto__']=[3,18];expect(draft.positions['__proto__']).toEqual([3,18]);expect(Object.getPrototypeOf(draft.positions)).toBe(null);
});
test('pinned plans contain only identifiers, models and expected snapshot revisions',()=>{
  const {plan}=setup();expect(plan.targets.map(target=>target.expectedRevision)).toEqual(['sample-r17','sample-r09']);expect(Object.isFrozen(plan.bindings[0])).toBe(true);expect(Object.keys(plan)).toEqual(['kind','inventoryVersion','tenantId','targets','bindings']);
});
test('read-only adapter exposes no apply/simulate operation',async()=>{
  const {inventory,plan}=setup(),client=createReadOnlyClient(inventory),preview=await client.preview(plan);expect(client.mode).toBe('read-only');expect('simulate' in client).toBe(false);expect('apply' in client).toBe(false);expect(preview.rows.map(row=>row.status)).toEqual(['inventory-ready','inventory-ready']);
});
test('explicit simulator reports independent sample success/conflict without mutations',async()=>{
  const {inventory,plan}=setup(),before=JSON.stringify(inventory),client=createSimulationClient(inventory),result=await client.simulate(plan);expect(result.rows.map(row=>row.status)).toEqual(['simulated-applied','simulated-conflict']);expect(JSON.stringify(inventory)).toBe(before);
});
test('missing models and local providers block just the affected target',async()=>{
  for(const [provider,model,expected] of [['openai','gpt-5.4','model-missing'],['local','local-model','provider-missing']]){
    const {inventory,plan}=setup(provider,model),client=createSimulationClient(inventory);expect((await client.preview(plan)).rows[1].status).toBe(expected);expect((await client.simulate(plan)).rows[1].status).toBe('simulated-skipped');
  }
});
test('client strips extra plan fields and refuses unsupported operations',async()=>{
  const {inventory,plan}=setup(),client=createReadOnlyClient(inventory);const result=await client.preview({...plan,credential:'SYNTHETIC-SECRET'});expect(JSON.stringify(result)).not.toContain('SYNTHETIC-SECRET');await expect(client.preview({...plan,kind:'grant-write'})).rejects.toThrow();
});
test('stale inventory, snapshots and aborted work cannot masquerade as current',async()=>{
  const {inventory,plan}=setup(),client=createReadOnlyClient(inventory);await expect(client.preview({...plan,inventoryVersion:'stale-version'})).rejects.toThrow();const changed=structuredClone(plan);changed.targets[0].expectedRevision='old';expect((await client.preview(changed)).rows[0].status).toBe('revision-conflict');const abort=new AbortController();abort.abort();await expect(client.preview(plan,{signal:abort.signal})).rejects.toThrow();
});
test('hostile result metadata never forwards bodies; unknown/duplicate targets or unpinned plans rejected',()=>{
  const {plan}=setup();const result={kind:'preview',inventoryVersion:plan.inventoryVersion,tenantId:plan.tenantId,plan,rows:plan.targets.map(target=>({id:target.id,status:'inventory-ready',body:'SYNTHETIC-SECRET'}))};expect(JSON.stringify(validateResult(plan,result,'preview'))).not.toContain('SYNTHETIC-SECRET');expect(()=>validateResult(plan,{...result,rows:[result.rows[0],result.rows[0]]},'preview')).toThrow();expect(()=>validateResult(plan,{...result,plan:{...plan,tenantId:'sandbox'}},'preview')).toThrow();expect(()=>validateResult(plan,{...result,rows:[{id:'east',status:'SYNTHETIC-SECRET'},result.rows[1]]},'preview')).toThrow();
});
test('inventory ownership, identity and relationships are validated before rendering',()=>{
  for(const mutate of [input=>input.tenants[0].resources.push(input.tenants[0].resources[0]),input=>input.tenants[0].links.push({from:'pi',to:'mac'}),input=>input.tenants[0].links.push({from:'compute',to:'govnr'}),input=>input.tenants[0].instances[0].providers[0].source='unknown',input=>input.tenants[0].instances[0].providers[0].models=['unlisted']]){
    const input=structuredClone(sampleInventory);mutate(input);expect(()=>normalizeInventory(input)).toThrow();
  }
});
test('Oqto host projection discards credentials and arbitrary fields, preserves stable references',()=>{
  const snapshot=structuredClone(sampleOqtoSnapshot);snapshot.tenants[0].machines[0].apiKey='SYNTHETIC-SECRET';snapshot.tenants[0].workspaces[0].credentialFile='/never/read/this';const value=projectOqtoInventory(snapshot);expect(JSON.stringify(value)).not.toContain('SYNTHETIC-SECRET');expect(JSON.stringify(value)).not.toContain('/never/read/this');expect(value.tenants[0].links).toEqual(sampleInventory.tenants[0].links);
});
