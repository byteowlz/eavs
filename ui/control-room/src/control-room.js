import { applyScheme } from '@byteowlz/design-system';
import { normalizeInventory, createDraft, addBinding, createPlan, classifyTargets, validateResult } from './model.js';
import { createReadOnlyClient } from './clients.js';
import { roomHTML, previewHTML, escapeHTML, statusLabels } from './render.js';
import { styles } from './styles.js';

let nextMount = 0;
/** Independent module; owns only one child, its shadow tree, and explicitly supplied operations. */
export function mountControlRoom(root, options) {
  if (!root?.ownerDocument || typeof root.append !== 'function') throw new Error('A DOM mount element is required');
  let inventory = normalizeInventory(options.inventory);
  let client = options.client || createReadOnlyClient(inventory);
  checkClient(client);
  let tenantId = options.tenantId || inventory.tenants[0].id;
  if (!inventory.tenants.some(tenant => tenant.id === tenantId)) throw new Error('Unknown tenant');
  const drafts = new Map();
  const doc = root.ownerDocument, win = doc.defaultView;
  const holder = doc.createElement('div');
  holder.className = 'eavs-control-room';
  holder.style.containerType = 'inline-size';
  holder.style.containerName = 'room';
  const shadow = holder.attachShadow({ mode: 'open' });
  const style = doc.createElement('style'); style.textContent = styles;
  const surface = doc.createElement('div');
  shadow.append(style, surface); root.append(holder);
  const listeners = new AbortController();
  const markerId = `room-arrow-${++nextMount}`;
  let disposed = false, generation = 0, pending = null, frame = 0, observer = null, preview = null, drag = null, moved = false;
  let themeProperties = [], onIntent = options.onIntent;
  const tenant = () => inventory.tenants.find(item => item.id === tenantId);
  const draft = () => { if (!drafts.has(tenantId)) drafts.set(tenantId, createDraft(tenant())); return drafts.get(tenantId); };
  const find = id => tenant().resources.find(resource => resource.id === id);
  const query = selector => surface.querySelector(selector);
  const announce = text => { const message = query('[data-message]'); if (message && !disposed) message.textContent = text; };

  function checkClient(candidate) {
    if (!candidate || !['read-only','simulation'].includes(candidate.mode) || typeof candidate.preview !== 'function' || (candidate.mode === 'simulation' && typeof candidate.simulate !== 'function')) throw new Error('Only read-only or explicit simulation adapters are supported');
  }
  function alive() { if (disposed) throw new Error('Control Room was destroyed'); }
  function invalidate() {
    generation++; pending?.abort(); pending = null; preview = null;
    query('[data-dialog]')?.close(); drag = null;
  }
  function emit() {
    if (!onIntent) return;
    try { onIntent(Object.freeze({ tenantId, inventoryVersion: inventory.version, targetIds: [...draft().targets].sort(), bindings: structuredClone(draft().bindings) })); }
    catch { announce('Host intent callback failed. No changes were applied.'); }
  }
  function changed(message) { invalidate(); render(); if (message) announce(message); emit(); }
  function validateTheme(theme) {
    if (!theme || theme === 'inherit') {
      const ambient = win.getComputedStyle(root);
      if (['--background','--foreground','--surface','--surface-sunken','--primary','--font-sans','--font-mono','--radius'].some(name => !ambient.getPropertyValue(name).trim())) throw new Error('Provide shared design-system host tokens or an explicit scoped theme.');
    } else applyScheme(theme.scheme, { root:doc.createElement('div'), identity:theme.identity, radius:theme.radius });
  }
  function setTheme(theme) {
    alive(); validateTheme(theme);
    for (const name of themeProperties) holder.style.removeProperty(name);
    holder.classList.remove('dark'); holder.removeAttribute('data-scheme-mode'); themeProperties = [];
    if (theme && theme !== 'inherit') {
      const before = new Set(Array.from(holder.style));
      applyScheme(theme.scheme, { root: holder, identity: theme.identity, radius: theme.radius });
      holder.dataset.schemeMode = theme.scheme.mode;
      themeProperties = Array.from(holder.style).filter(name => !before.has(name));
    }
    scheduleWires();
  }
  function scheduleWires() { if (disposed) return; win.cancelAnimationFrame(frame); frame = win.requestAnimationFrame(drawWires); }
  function drawWires() {
    if (disposed) return;
    const graph = query('[data-graph]'), svg = query('.wires'); if (!graph || !svg) return;
    const bounds = graph.getBoundingClientRect();
    svg.setAttribute('viewBox', `0 0 ${bounds.width} ${bounds.height}`);
    const edges = [...tenant().links, ...draft().bindings.map(binding => ({ from: binding.from, to: binding.provider, draft: true }))];
    svg.innerHTML = `<defs><marker id="${markerId}" markerWidth="7" markerHeight="7" refX="6" refY="3.5" orient="auto"><path d="M0 0 7 3.5 0 7z" fill="currentColor"/></marker></defs>` + edges.map(edge => {
      const from = query(`[data-node="${edge.from}"]`)?.getBoundingClientRect(), to = query(`[data-node="${edge.to}"]`)?.getBoundingClientRect();
      if (!from || !to) return '';
      const x1 = from.right - bounds.left, y1 = from.top + from.height / 2 - bounds.top, x2 = to.left - bounds.left, y2 = to.top + to.height / 2 - bounds.top, bend = Math.max(35,(x2 - x1) / 2);
      return `<path class="wire ${edge.draft ? 'draft' : ''}" d="M${x1} ${y1} C${x1+bend} ${y1},${x2-bend} ${y2},${x2} ${y2}" marker-end="url(#${markerId})"/>`;
    }).join('');
  }
  function render() {
    if (disposed) return;
    observer?.disconnect(); query('[data-dialog]')?.close();
    surface.innerHTML = roomHTML(inventory, tenant(), draft(), client.mode);
    observer = new win.ResizeObserver(scheduleWires); observer.observe(query('[data-graph]'));
    scheduleWires();
  }
  function stage(from, providerId, model) {
    try {
      if (!addBinding(tenant(), draft(), from, providerId, model)) { announce('That resource, provider and model are already staged.'); return; }
      changed('Connection staged locally. No gateway was updated.');
    } catch { announce('Choose a valid source, provider and declared model.'); }
  }
  async function showPreview() {
    let plan;
    try { plan = createPlan(inventory, tenantId, draft()); }
    catch { announce('Choose explicit target instances and valid access intents first.'); return; }
    invalidate(); const ticket = generation; const request = new AbortController(); pending = request;
    announce('Checking the pinned inventory plan…');
    try {
      const raw = await client.preview(plan, { signal: request.signal });
      if (disposed || ticket !== generation || request.signal.aborted) return;
      const result = validateResult(plan, raw, 'preview');
      const localRows = classifyTargets(inventory, plan);
      if (result.rows.some(row => row.status === 'inventory-ready' && localRows.find(local => local.id === row.id).status !== 'inventory-ready')) throw new Error('Inconsistent inventory verdict');
      pending = null; preview = { plan, result, generation: ticket, simulated: false };
      const dialog = query('[data-dialog]');
      dialog.innerHTML = previewHTML(tenant(), plan, result, client.mode === 'simulation');
      dialog.showModal(); announce('Inventory preview only. Authorization is not established by this UI.');
    } catch {
      if (disposed || ticket !== generation || request.signal.aborted) return;
      pending = null; announce('Preview rejected. Refresh inventory and review the plan. No updates were sent.');
    }
  }
  async function simulate() {
    if (!preview || preview.simulated || preview.generation !== generation || client.mode !== 'simulation') return;
    const pinned = preview; pinned.simulated = true;
    const request = new AbortController(); pending = request;
    query('[data-action="simulate"]').disabled = true;
    try {
      const raw = await client.simulate(pinned.plan, { signal: request.signal });
      if (disposed || pinned.generation !== generation || request.signal.aborted || preview !== pinned) return;
      const result = validateResult(pinned.plan, raw, 'simulation');
      if (result.rows.some(row => row.status !== 'simulated-skipped' && pinned.result.rows.find(previous => previous.id === row.id).status !== 'inventory-ready')) throw new Error('Blocked target receipt');
      query('[data-receipts]').innerHTML = '<p class="eyebrow">SIMULATED RECEIPTS · DRAFT RETAINED</p>' + result.rows.map(row => `<div class="receipt ${row.status === 'simulated-applied' ? '' : 'blocked'}"><b>${escapeHTML(tenant().instances.find(instance => instance.id === row.id).name)}</b><span>${statusLabels[row.status]}</span></div>`).join('');
      pending = null; announce('Simulation completed. No real requests, grants or credentials were written.');
    } catch {
      if (disposed || pinned.generation !== generation || request.signal.aborted) return;
      pending = null; announce('Simulation response rejected. No retry will occur.');
    }
  }

  surface.addEventListener('change', event => {
    const element = event.target;
    if (element.matches('[data-tenant]')) { if (!inventory.tenants.some(item => item.id === element.value)) return; tenantId = element.value; changed('Tenant context changed. Drafts and targets are isolated.'); }
    else if (element.matches('[data-target]')) { if (!tenant().instances.some(instance => instance.id === element.dataset.target && instance.available)) { element.checked = false; return; } element.checked ? draft().targets.add(element.dataset.target) : draft().targets.delete(element.dataset.target); changed(); }
    else if (element.matches('[data-provider]')) {
      const provider = find(element.value); if (provider?.type !== 'provider') return;
      query('[data-model]').innerHTML = provider.models.map(model => `<option value="${escapeHTML(model)}">${escapeHTML(model)}</option>`).join('');
      query('[data-action="stage"]').disabled = !provider.models.length;
    }
  }, { signal: listeners.signal });
  surface.addEventListener('click', event => {
    const element = event.target.closest('button,[data-node]'); if (!element) return; if (moved) { moved = false; return; }
    if (element.hasAttribute('data-port')) {
      const selected = find(element.dataset.port); if (!selected) return;
      if (selected.type === 'provider') stage(draft().armed, selected.id, selected.models[0]);
      else { draft().selected = selected.id; draft().armed = selected.id; render(); }
    } else if (element.hasAttribute('data-node') || element.hasAttribute('data-select')) { const id = element.dataset.node || element.dataset.select; if (!find(id)) return; draft().selected = id; render(); }
    else if (element.hasAttribute('data-remove')) { draft().history.push(structuredClone(draft().bindings)); draft().bindings.splice(Number(element.dataset.remove),1); changed(); }
    else {
      switch (element.dataset.action) {
        case 'connect': if (find(draft().selected)?.type !== 'provider') { draft().armed = draft().selected; render(); } else announce('Select a source resource first.'); break;
        case 'fit': draft().positions = Object.create(null); render(); break;
        case 'stage': stage(draft().selected, query('[data-provider]')?.value, query('[data-model]')?.value); break;
        case 'undo': if (draft().history.length) { draft().bindings = draft().history.pop(); changed(); } break;
        case 'discard': draft().history.push(structuredClone(draft().bindings)); draft().bindings = []; draft().armed = null; changed('Draft discarded; no writes.'); break;
        case 'preview': void showPreview(); break;
        case 'simulate': void simulate(); break;
        case 'close': invalidate(); break;
      }
    }
  }, { signal: listeners.signal });
  surface.addEventListener('keydown', event => {
    if (event.target.closest('button')) return;
    const element = event.target.closest('[data-node]');
    if (element && ['Enter',' '].includes(event.key)) { event.preventDefault(); draft().selected = element.dataset.node; render(); }
  }, { signal: listeners.signal });
  surface.addEventListener('cancel', () => invalidate(), { capture: true, signal: listeners.signal });
  surface.addEventListener('pointerdown', event => {
    if (event.button !== 0 || (event.target.closest('button') && !event.target.closest('.node-content'))) return;
    const element = event.target.closest('[data-node]'); if (!element) return;
    moved = false; const graph = query('[data-graph]');
    drag = { element, graph, x: event.clientX, y: event.clientY, left: element.offsetLeft, top: element.offsetTop };
    element.setPointerCapture(event.pointerId);
  }, { signal: listeners.signal });
  surface.addEventListener('pointermove', event => {
    if (!drag) return; const dx = event.clientX-drag.x, dy = event.clientY-drag.y;
    if (Math.abs(dx)+Math.abs(dy) < 5) return; moved = true;
    const width = drag.graph.clientWidth, height = drag.graph.clientHeight;
    const x = Math.max(0,Math.min(width-drag.element.offsetWidth,drag.left+dx)), y = Math.max(0,Math.min(height-drag.element.offsetHeight-25,drag.top+dy));
    drag.element.style.left = `${x}px`; drag.element.style.top = `${y}px`;
    draft().positions[drag.element.dataset.node] = [x/width*100,y/height*100]; drawWires();
  }, { signal: listeners.signal });
  surface.addEventListener('pointerup', () => { drag = null; }, { signal: listeners.signal });
  surface.addEventListener('pointercancel', () => { drag = null; moved = false; }, { signal: listeners.signal });

  try { setTheme(options.theme || 'inherit'); render(); } catch (error) { listeners.abort(); holder.remove(); throw error; }
  return Object.freeze({
    update(next) {
      alive(); const updated = next.inventory ? normalizeInventory(next.inventory) : inventory;
      const inventoryChanged = Boolean(next.inventory) && JSON.stringify(updated) !== JSON.stringify(inventory);
      const nextClient = next.client || (inventoryChanged ? createReadOnlyClient(updated) : client); checkClient(nextClient);
      const nextTenant = next.tenantId || (updated.tenants.some(tenant => tenant.id === tenantId) ? tenantId : updated.tenants[0].id);
      if (!updated.tenants.some(tenant => tenant.id === nextTenant)) throw new Error('Unknown tenant');
      const contextChanged = nextTenant !== tenantId;
      if ('theme' in next) validateTheme(next.theme);
      invalidate();
      if (inventoryChanged) drafts.clear(); // Changed snapshots never silently preserve stale revisions/intents.
      inventory = updated; client = nextClient; tenantId = nextTenant;
      if ('onIntent' in next) onIntent = next.onIntent;
      if ('theme' in next) setTheme(next.theme);
      render(); if (inventoryChanged || contextChanged) emit();
    },
    setTheme,
    destroy() {
      if (disposed) return;
      invalidate(); disposed = true; listeners.abort(); observer?.disconnect(); win.cancelAnimationFrame(frame); drafts.clear(); holder.remove();
    }
  });
}
