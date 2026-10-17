import { normalizeInventory } from './model.js';
import { mountControlRoom } from './control-room.js';

/**
 * Host projection seam, NOT an implementation of Oqto's inventory endpoints.
 * The host supplies authorized tenant/instance/provider inventory and stable resource IDs.
 * Credentials, arbitrary host attributes and harness config are never forwarded.
 */
export function projectOqtoInventory(snapshot) {
  return normalizeInventory({ version: snapshot.version, tenants: snapshot.tenants.map(tenant => ({
    id: tenant.id, name: tenant.name, instances: tenant.instances,
    resources: [
      ...tenant.machines.map(machine => ({ id: machine.id, type:'machine', name:machine.name, detail:'Oqto machine' })),
      ...tenant.workspaces.map(workspace => ({ id: workspace.id, type:'workspace', name:workspace.name, detail:'Oqto workspace' })),
      ...tenant.harnesses.map(harness => ({ id: harness.id, type:'harness', name:harness.name, detail:'Agent harness · context only' })),
      ...tenant.providers.map(provider => ({ id:provider.id, type:'provider', name:provider.name, detail:provider.models[0] || 'No models declared', models:provider.models }))
    ],
    links: [
      ...tenant.workspaces.filter(workspace => workspace.machineId).map(workspace => ({from:workspace.machineId,to:workspace.id})),
      ...tenant.harnesses.filter(harness => harness.workspaceId).map(harness => ({from:harness.workspaceId,to:harness.id}))
    ]
  })) });
}
export function mountOqtoControlRoom(root, { snapshot, client, onIntent, tenantId }) {
  const module = mountControlRoom(root, { inventory:projectOqtoInventory(snapshot), client, onIntent, tenantId, theme:'inherit' });
  return Object.freeze({
    update({ snapshot, ...options }) { module.update({ ...options, ...(snapshot ? { inventory:projectOqtoInventory(snapshot) } : {}), theme:'inherit' }); },
    destroy:module.destroy
  });
}
