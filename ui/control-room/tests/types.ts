import { mountControlRoom, normalizeInventory, createReadOnlyClient, createSimulationClient, controlRoomScheme, projectOqtoInventory, type Inventory, type Options, type ReadOnlyClient } from '../src/index.js';
const inventory:Inventory = { version:'v1',tenants:[{id:'t1',name:'Tenant',resources:[{id:'p1',type:'provider',name:'Provider',models:['physical-model']}],links:[],instances:[{id:'i1',name:'Instance',revision:'r1',available:true,providers:[{id:'p1',source:'toml',models:['physical-model']}]}]}] };
const normalized = normalizeInventory(inventory);
// @ts-expect-error Intentional negative contract test: normalized snapshot is deeply readonly.
normalized.tenants[0].instances[0].revision = 'changed';
const client:ReadOnlyClient = createReadOnlyClient(inventory);
// @ts-expect-error Intentional negative contract test: read-only clients expose no simulate operation.
client.simulate({});
const options:Options = { inventory, client, theme:{scheme:controlRoomScheme,radius:'8px'} };
const root=document.createElement('div');
const module=mountControlRoom(root,options);module.update({client:createSimulationClient(inventory)});module.setTheme('inherit');module.destroy();
projectOqtoInventory({version:'v1',tenants:[]});
// @ts-expect-error Intentional negative contract test: providers require their physical model list.
const provider:Inventory['tenants'][number]['resources'][number]={id:'p1',type:'provider',name:'No models'};
void provider;
