import { applyScheme, oqtoDark, oqtoLight, nordBase16 } from '../src/theme.js';
import { mountControlRoom, mountOqtoControlRoom, createSimulationClient, controlRoomScheme, controlRoomIdentity } from '../src/index.js';
import { sampleInventory, sampleOqtoSnapshot } from './inventory.js';

const embedded = document.body.dataset.demo === 'embedded';
const shell = document.querySelector('[data-shell]');
const root = document.querySelector('[data-mount]');
const schemes = { eavs:controlRoomScheme, 'oqto-dark':oqtoDark, 'oqto-light':oqtoLight, nord:nordBase16 };
let module;
function mount() {
  module = embedded ? mountOqtoControlRoom(root, { snapshot:sampleOqtoSnapshot }) : mountControlRoom(root, { inventory:sampleInventory, client:createSimulationClient(sampleInventory) });
}
function theme() {
  const scheme = schemes[document.querySelector('[data-scheme]').value];
  applyScheme(scheme, { root:shell, identity:controlRoomIdentity, radius:`${document.querySelector('[data-radius]').value}px` });
  document.querySelector('[data-radius-label]').textContent = `${document.querySelector('[data-radius]').value}px`;
}
if (embedded) { document.querySelector('[data-scheme]').value = 'oqto-dark'; document.querySelector('[data-radius]').value = '0'; }
theme(); mount();
document.querySelector('[data-scheme]').onchange = theme;
document.querySelector('[data-radius]').oninput = theme;
document.querySelector('[data-lifecycle]').onclick = () => {
  if (module) { module.destroy(); module = null; document.querySelector('[data-lifecycle]').textContent = 'Remount module'; }
  else { mount(); document.querySelector('[data-lifecycle]').textContent = 'Destroy module'; }
};
// Explicit synthetic demonstration/test hooks only; absent from the reusable package.
window.controlRoomDemo = { root, shell, inventory:sampleInventory, oqtoSnapshot:sampleOqtoSnapshot, mountControlRoom, mountOqtoControlRoom, createSimulationClient, get module(){ return module; } };
