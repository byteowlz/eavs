// Optional verification using explicit already-installed/cached packages. Never installs.
import { readFile, copyFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
const root=fileURLToPath(new URL('../',import.meta.url));
const paths={react:process.env.REACT_PACKAGE_DIR,dom:process.env.REACT_DOM_PACKAGE_DIR,scheduler:process.env.SCHEDULER_PACKAGE_DIR};
if(Object.values(paths).some(value=>!value))throw new Error('Supply existing REACT_PACKAGE_DIR, REACT_DOM_PACKAGE_DIR and SCHEDULER_PACKAGE_DIR; no installation performed');
for(const [key,path] of Object.entries(paths)){const pkg=JSON.parse(await readFile(join(path,'package.json'),'utf8'));if(pkg.name!==({react:'react',dom:'react-dom',scheduler:'scheduler'})[key])throw new Error('Unexpected verification package');}
const plugin={name:'existing-proof-dependencies',setup(builder){
  builder.onResolve({filter:/^react$/},()=>({path:join(paths.react,'index.js')}));
  builder.onResolve({filter:/^react-dom\/client$/},()=>({path:join(paths.dom,'client.js')}));
  builder.onResolve({filter:/^react-dom$/},()=>({path:join(paths.dom,'index.js')}));
  builder.onResolve({filter:/^scheduler$/},()=>({path:join(paths.scheduler,'index.js')}));
  builder.onResolve({filter:/^@byteowlz\/design-system$/},()=>({path:join(process.env.DESIGN_SYSTEM_DIR || join(root,'../../../design-system'),'impls/shadcn-ts/src/index.ts')}));
}};
const result=await Bun.build({entrypoints:[join(root,'tests/react-proof.js')],outdir:join(root,'dist'),naming:'react-proof.js',target:'browser',format:'esm',plugins:[plugin]});
if(!result.success){for(const log of result.logs)console.error(log);throw new Error('React verification bundle failed');}
await writeFile(join(root,'dist/react-proof.html'),'<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>React bridge lifecycle proof · synthetic</title><link rel="stylesheet" href="demo.css"><script type="module" src="react-proof.js"></script></head><body><main class="shell" data-react-host></main></body></html>');
for(const [name,path] of [['react-LICENSE.txt',paths.react],['react-dom-LICENSE.txt',paths.dom]])await copyFile(join(path,'LICENSE'),join(root,'dist/assets',name));
console.log('Built optional actual React StrictMode lifecycle proof from existing packages; core stays React-free.');
