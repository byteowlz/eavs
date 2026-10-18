export const sampleInventory = {
  version:'sample-inventory-01',
  tenants:[
    { id:'studio', name:'Personal studio', resources:[
      {id:'mac',type:'machine',name:'Atelier Mac',detail:'macOS · machine baseline'},
      {id:'compute',type:'machine',name:'Compute 01',detail:'Linux · machine baseline'},
      {id:'govnr',type:'workspace',name:'Oqto / govnr',detail:'Workspace provider pool'},
      {id:'research',type:'workspace',name:'Oqto / Research',detail:'Workspace provider pool'},
      {id:'pi',type:'harness',name:'Pi / researcher',detail:'Agent harness · sample'},
      {id:'codex',type:'harness',name:'Codex / builder',detail:'Agent harness · sample'},
      {id:'anthropic',type:'provider',name:'Anthropic',detail:'claude-haiku-5-5',models:['claude-haiku-5-5']},
      {id:'openai',type:'provider',name:'OpenAI',detail:'gpt-5.4-mini',models:['gpt-5.4-mini','gpt-5.4']},
      {id:'local',type:'provider',name:'Local inference',detail:'TOML · owner-local',models:['local-model']}
    ], links:[{from:'mac',to:'govnr'},{from:'compute',to:'research'},{from:'govnr',to:'pi'},{from:'research',to:'codex'}], instances:[
      {id:'east',name:'eavs-east',revision:'sample-r17',available:true,providers:[{id:'anthropic',source:'stored',models:['claude-haiku-5-5']},{id:'openai',source:'stored',models:['gpt-5.4-mini','gpt-5.4']},{id:'local',source:'toml',models:['local-model']}]},
      {id:'west',name:'eavs-west',revision:'sample-r09',available:true,providers:[{id:'anthropic',source:'toml',models:['claude-haiku-5-5']},{id:'openai',source:'stored',models:['gpt-5.4-mini']}]},
      {id:'travel',name:'eavs-travel',revision:'sample-r00',available:false,providers:[]}
    ]},
    {id:'sandbox',name:'Sandbox',resources:[{id:'vm',type:'machine',name:'Sandbox VM',detail:'Disposable machine'},{id:'playground',type:'workspace',name:'Oqto / Playground',detail:'Workspace pool'},{id:'explorer',type:'harness',name:'Pi / explorer',detail:'Harness scope'},{id:'openai',type:'provider',name:'OpenAI',detail:'gpt-5.4-mini',models:['gpt-5.4-mini']}],links:[{from:'vm',to:'playground'},{from:'playground',to:'explorer'}],instances:[{id:'sandbox',name:'eavs-sandbox',revision:'sample-r03',available:true,providers:[{id:'openai',source:'stored',models:['gpt-5.4-mini']}]}]}
  ]
};
export const sampleOqtoSnapshot = {
  version:sampleInventory.version,
  tenants:sampleInventory.tenants.map(tenant => ({
    id:tenant.id,name:tenant.name,instances:tenant.instances,
    machines:tenant.resources.filter(resource => resource.type === 'machine'),
    workspaces:tenant.resources.filter(resource => resource.type === 'workspace').map(resource => ({...resource,machineId:tenant.links.find(link => link.to === resource.id)?.from})),
    harnesses:tenant.resources.filter(resource => resource.type === 'harness').map(resource => ({...resource,workspaceId:tenant.links.find(link => link.to === resource.id)?.from})),
    providers:tenant.resources.filter(resource => resource.type === 'provider')
  }))
};
