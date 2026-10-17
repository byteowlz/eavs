/** Allowlisted inventory and immutable policy-intent planning. No credential fields. */
const ID = /^[a-zA-Z0-9._:-]{1,96}$/;
const TYPES = new Set(['machine', 'workspace', 'harness', 'provider']);
function id(value) { if (typeof value !== 'string' || !ID.test(value)) throw new Error('Invalid identifier'); return value; }
function text(value, max = 160) { if (typeof value !== 'string' || !value.length || value.length > max || /[\x00-\x1f\x7f]/.test(value)) throw new Error('Invalid display text'); return value; }
function list(value, max = 200) { if (!Array.isArray(value) || value.length > max) throw new Error('Invalid inventory list'); return value; }
function unique(items, key = 'id') { if (new Set(items.map(item => item[key])).size !== items.length) throw new Error('Duplicate identifier'); return items; }
function freeze(value) { if (value && typeof value === 'object') { Object.values(value).forEach(freeze); Object.freeze(value); } return value; }

export function normalizeInventory(input) {
  const version = id(input.version);
  const tenants = unique(list(input.tenants, 30).map(tenant => {
    const resources = unique(list(tenant.resources, 120).map(resource => {
      if (!TYPES.has(resource.type)) throw new Error('Unknown resource type');
      return { id: id(resource.id), type: resource.type, name: text(resource.name), detail: text(resource.detail || resource.type), models: resource.type === 'provider' ? unique(list(resource.models, 100).map(model => ({ id: text(model, 200) }))).map(model => model.id) : [] };
    }));
    const byId = new Map(resources.map(resource => [resource.id, resource]));
    const links = list(tenant.links, 240).map(link => {
      const from = byId.get(id(link.from)), to = byId.get(id(link.to));
      if (!from || !to || !((from.type === 'machine' && to.type === 'workspace') || (from.type === 'workspace' && to.type === 'harness'))) throw new Error('Invalid inventory relationship');
      return { from: from.id, to: to.id };
    });
    if (new Set(links.map(link => `${link.from}/${link.to}`)).size !== links.length || new Set(links.map(link => link.to)).size !== links.length) throw new Error('Duplicate resource parent');
    const instances = unique(list(tenant.instances, 40).map(instance => ({
      id: id(instance.id), name: text(instance.name), revision: id(instance.revision), available: instance.available === true,
      providers: unique(list(instance.providers, 120).map(provider => {
        const resource = byId.get(id(provider.id));
        if (resource?.type !== 'provider' || !['stored', 'toml'].includes(provider.source)) throw new Error('Invalid provider declaration');
        const models = unique(list(provider.models, 100).map(model => ({ id: text(model, 200) }))).map(model => model.id);
        if (models.some(model => !resource.models.includes(model))) throw new Error('Unknown declared model');
        return { id: resource.id, source: provider.source, models };
      }))
    })));
    return { id: id(tenant.id), name: text(tenant.name), resources, links, instances };
  }));
  if (!tenants.length) throw new Error('At least one tenant is required');
  return freeze({ version, tenants });
}

export function createDraft(tenant) {
  return { targets: new Set(), bindings: [], selected: tenant.resources.find(resource => resource.type === 'harness')?.id || tenant.resources[0]?.id, armed: null, positions: Object.create(null), history: [] };
}
export function addBinding(tenant, draft, from, to, model) {
  const source = tenant.resources.find(resource => resource.id === from), provider = tenant.resources.find(resource => resource.id === to);
  if (!source || source.type === 'provider' || provider?.type !== 'provider' || !provider.models.includes(model)) throw new Error('Invalid policy intent');
  if (draft.bindings.some(binding => binding.from === from && binding.provider === to && binding.model === model)) return false;
  draft.history.push(structuredClone(draft.bindings));
  draft.bindings.push({ from, provider: to, model });
  draft.armed = null;
  return true;
}
export function createPlan(inventory, tenantId, draft) {
  const tenant = inventory.tenants.find(tenant => tenant.id === tenantId);
  if (!tenant || !draft.targets.size || !draft.bindings.length) throw new Error('Explicit targets and intents required');
  const targets = [...draft.targets].sort().map(target => {
    const instance = tenant.instances.find(instance => instance.id === target);
    if (!instance?.available) throw new Error('Unavailable or foreign target');
    return { id: instance.id, expectedRevision: instance.revision };
  });
  const check = createDraft(tenant);
  for (const binding of draft.bindings) if (!addBinding(tenant, check, binding.from, binding.provider, binding.model)) throw new Error('Duplicate policy intent');
  return freeze({ kind: 'policy-intent', inventoryVersion: inventory.version, tenantId, targets, bindings: check.bindings });
}
export function normalizePlan(inventory, raw) {
  if (raw.kind !== 'policy-intent') throw new Error('Unsupported operation');
  const tenant = inventory.tenants.find(tenant => tenant.id === id(raw.tenantId));
  if (!tenant) throw new Error('Foreign tenant');
  const targets = unique(list(raw.targets, 40).map(target => {
    if (!tenant.instances.some(instance => instance.id === id(target.id))) throw new Error('Foreign target');
    return { id:target.id, expectedRevision:id(target.expectedRevision) };
  }));
  const draft = createDraft(tenant);
  for (const binding of list(raw.bindings, 120)) if (!addBinding(tenant, draft, binding.from, binding.provider, binding.model)) throw new Error('Duplicate intent');
  if (!targets.length || !draft.bindings.length) throw new Error('Empty plan');
  return freeze({kind:'policy-intent',inventoryVersion:id(raw.inventoryVersion),tenantId:tenant.id,targets,bindings:draft.bindings});
}
export function classifyTargets(inventory, plan) {
  const tenant = inventory.tenants.find(tenant => tenant.id === plan.tenantId);
  if (!tenant || inventory.version !== plan.inventoryVersion) throw new Error('Stale inventory');
  return plan.targets.map(target => {
    const instance = tenant.instances.find(instance => instance.id === target.id);
    let status = 'inventory-ready';
    if (!instance?.available) status = 'unavailable';
    else if (instance.revision !== target.expectedRevision) status = 'revision-conflict';
    else for (const binding of plan.bindings) {
      const provider = instance.providers.find(provider => provider.id === binding.provider);
      if (!provider) { status = 'provider-missing'; break; }
      if (!provider.models.includes(binding.model)) { status = 'model-missing'; break; }
    }
    return { id: target.id, status };
  });
}
export function validateResult(plan, result, kind) {
  const allowed = kind === 'preview' ? new Set(['inventory-ready', 'unavailable', 'revision-conflict', 'provider-missing', 'model-missing']) : new Set(['simulated-applied', 'simulated-conflict', 'simulated-skipped']);
  if (result?.kind !== kind || result.inventoryVersion !== plan.inventoryVersion || result.tenantId !== plan.tenantId || JSON.stringify(result.plan) !== JSON.stringify(plan)) throw new Error('Unpinned result');
  const rows = unique(list(result.rows, 40).map(row => { if (!allowed.has(row.status) || !plan.targets.some(target => target.id === row.id)) throw new Error('Invalid target result'); return { id: row.id, status: row.status }; }));
  if (rows.length !== plan.targets.length) throw new Error('Incomplete target result');
  return freeze({ kind, rows });
}
