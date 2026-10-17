import { classifyTargets, normalizeInventory, normalizePlan, validateResult } from './model.js';

function result(kind, inventory, plan, rows) {
  const value = { kind, inventoryVersion: inventory.version, tenantId: plan.tenantId, plan, rows };
  validateResult(plan, value, kind);
  return value;
}
function aborted(signal) { if (signal?.aborted) throw new DOMException('Aborted', 'AbortError'); }

/** Inventory-only planning. This does not assert server authorization or compatibility. */
export function createReadOnlyClient(input) {
  const inventory = normalizeInventory(input);
  return Object.freeze({
    mode: 'read-only',
    async preview(plan, { signal } = {}) {
      aborted(signal); plan = normalizePlan(inventory, plan);
      return result('preview', inventory, plan, classifyTargets(inventory, plan));
    }
  });
}

/** Explicit offline simulator: no fetch, provider calls, configuration or credential writes. */
export function createSimulationClient(input) {
  const inventory = normalizeInventory(input);
  return Object.freeze({
    mode: 'simulation',
    async preview(plan, { signal } = {}) {
      aborted(signal); plan = normalizePlan(inventory, plan);
      return result('preview', inventory, plan, classifyTargets(inventory, plan));
    },
    async simulate(plan, { signal } = {}) {
      aborted(signal); plan = normalizePlan(inventory, plan);
      const rows = classifyTargets(inventory, plan).map((row, index) => ({
        id: row.id,
        status: row.status !== 'inventory-ready' ? 'simulated-skipped' : index === 1 ? 'simulated-conflict' : 'simulated-applied'
      }));
      return result('simulation', inventory, plan, rows);
    }
  });
}
