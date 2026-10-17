// Explicit existing compiler + installed Pi SDK, no install, no vendor lint waiver.
import {mkdtemp,rm,stat} from "node:fs/promises";
import {writeFile} from "node:fs/promises";
import {tmpdir} from "node:os";
import {dirname,join,resolve} from "node:path";
import {fileURLToPath} from "node:url";
import {spawnSync} from "node:child_process";
const [compilerArg,piArg]=process.argv.slice(2);
if (!compilerArg || !piArg) throw new Error("usage: node scripts/check_pi_extension_types.mjs TYPESCRIPT_LIB_TSC_JS INSTALLED_PI_PACKAGE");
const compiler=resolve(compilerArg),pi=resolve(piArg),modules=resolve(dirname(compiler),"../..");
const directory=await mkdtemp(join(tmpdir(),"eavs-pi-types-"));
try {
 const paths={"@earendil-works/pi-coding-agent":[join(pi,"dist/index.d.ts")]};
 // Some installed Pi SDKs have an optional Gemini DTS MCP peer. Resolve an
 // explicitly preprovisioned REAL peer; never fabricate declarations or install it.
 const peer=join(modules,"@modelcontextprotocol/sdk/dist/esm");
 if (await stat(peer).then(v=>v.isDirectory(),()=>false)) paths["@modelcontextprotocol/sdk/*"]=[join(peer,"*")];
 const source=fileURLToPath(new URL("../adapters/pi/",import.meta.url));
 const config={compilerOptions:{target:"ES2022",module:"ESNext",moduleResolution:"Bundler",resolveJsonModule:true,strict:true,noEmit:true,allowImportingTsExtensions:true,types:["node"],typeRoots:[join(modules,"@types"),join(pi,"node_modules/@types")],baseUrl:directory,paths},files:["provider-library.ts","live-providers.ts","extension.ts"].map(name=>join(source,name))};
 const path=join(directory,"tsconfig.json");await writeFile(path,JSON.stringify(config));
 // Pi loads TS through jiti. Bundler resolution matches that loader; vendor
 // declarations are still checked (no skipLibCheck). Node runs TypeScript itself.
 const child=spawnSync(process.execPath,[compiler,"-p",path],{stdio:"inherit",timeout:120_000});
 if (child.error || child.status!==0) process.exitCode=1;
 else console.log("Pi extension + provider library: strict TypeScript and actual Pi SDK declarations pass.");
} finally {await rm(directory,{recursive:true,force:true});}
