import { mkdir, copyFile, readFile, readdir, writeFile, lstat } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';

const root = fileURLToPath(new URL('.', import.meta.url));
const designRoot = resolve(process.env.DESIGN_SYSTEM_DIR || join(root,'../../../design-system'));
const designPackage = JSON.parse(await readFile(join(designRoot,'impls/shadcn-ts/package.json'),'utf8'));
if (designPackage.name !== '@byteowlz/design-system') throw new Error('Expected the shared design-system workspace');
const designEntry = join(designRoot,'impls/shadcn-ts/src/index.ts');
const plugin = { name:'shared-design-system', setup(builder) { builder.onResolve({filter:/^@byteowlz\/design-system$/}, () => ({path:designEntry})); } };
const dist = join(root,'dist');
await mkdir(join(dist,'assets'),{recursive:true});
for (const [entry, output, external] of [['src/index.js','control-room.js',[]],['src/react.js','react.js',['react']],['demo/demo.js','demo.js',[]]]) {
  const result = await Bun.build({ entrypoints:[join(root,entry)], outdir:dist, naming:output, target:'browser',format:'esm',external,plugins:[plugin] });
  if (!result.success) { for (const log of result.logs) console.error(log); throw new Error('UI bundle failed'); }
}
for (const name of ['index.html','embedded.html','demo.css']) await copyFile(join(root,'demo',name),join(dist,name));
for (const name of ['index.d.ts','react.d.ts']) await copyFile(join(root,'src',name),join(dist,name));
for (const name of ['manrope.ttf','mono.ttf','manrope-OFL.txt','ibmplexmono-OFL.txt','favicon.svg']) {
  const source=resolve(root,'../../mockups/graph-admin/assets',name);
  if (!(await lstat(source)).isFile()) throw new Error('Expected a regular self-hosted asset, not a reference');
  await copyFile(source,join(dist,'assets',name));
}
const hash = createHash('sha256');
async function fingerprint(directory) {
  for (const entry of (await readdir(directory,{withFileTypes:true})).sort((a,b)=>a.name.localeCompare(b.name))) {
    const path = join(directory,entry.name);
    if (entry.name.startsWith('.')) continue;
    if (entry.isDirectory()) await fingerprint(path);
    else if (entry.isFile() && /\.(ts|json)$/.test(entry.name)) { hash.update(path.slice(designRoot.length)); hash.update(await readFile(path)); }
  }
}
await fingerprint(join(designRoot,'impls/shadcn-ts/src'));
await writeFile(join(dist,'build-manifest.json'),JSON.stringify({ dependency:'@byteowlz/design-system',version:designPackage.version,sourceSHA256:hash.digest('hex'),source:'local workspace build, not a registry publication',liveWrites:false },null,2)+'\n');
console.log('Built reusable module, optional React bridge, standalone and embedded demos from the actual shared design-system source.');
