import {test,expect} from "bun:test";
import {mkdtemp,writeFile,readFile,rm} from "node:fs/promises";
import {createServer} from "node:http";
import {join} from "node:path";
import {tmpdir} from "node:os";
import extension from "./extension.ts";
import {isolatedProxyEnvironment} from "./test-environment.ts";
isolatedProxyEnvironment();

test("extension registers before startup, refreshes/prunes only owned metadata and never writes config",async()=>{
 const directory=await mkdtemp(join(tmpdir(),"eavs-extension-"));const path=join(directory,"device.json");
 const keyRef=`EAVS_EXTENSION_TEST_${crypto.randomUUID().replaceAll("-","")}`;
 process.env[keyRef]="eavs-synthetic000000000000000000000000000000";
 let model="model-a",deny=false,empty=false,calls=0;
 const server=createServer((request,response)=>{
  calls++;
  if (deny || request.headers.authorization!==`Bearer ${process.env[keyRef]}`){response.writeHead(401);response.end("private-provider-secret");return;}
  response.end(JSON.stringify({schema_version:1,scope:"key-filtered-chat-candidates",providers:empty?[]:[{id:"alpha",api:"openai-responses",base_path:"/alpha/v1",request_path:"/alpha/v1/responses",operation:"chat",models:[{id:model,name:"Test model",reasoning:true,input:["text"],context_window:128_000,max_tokens:4096,pricing:{status:"unverified_estimate",unit:"USD_per_million_tokens",rates:{input:1,output:2,cache_read:0.5,cache_write:1.25}}}]}]}));
 });
 await new Promise<void>(resolve=>server.listen(0,"127.0.0.1",resolve));const address=server.address();if(!address || typeof address==="string") throw new Error("fixture address");
 const original=JSON.stringify({connections:[{name:"fixture",url:`http://127.0.0.1:${address.port}`,key_env:keyRef,allow_loopback_http:true}]});await writeFile(path,original);
 const providers=new Map<string,any>([["unrelated",{keep:true}]]),events=new Map<string,Function>(),commands=new Map<string,any>();
 const api={registerFlag:()=>{},getFlag:(name:string)=>name==="eavs-device"?"fixture":path,registerProvider:(name:string,value:any)=>providers.set(name,value),unregisterProvider:(name:string)=>providers.delete(name),registerCommand:(name:string,value:any)=>commands.set(name,value),on:(name:string,fn:Function)=>events.set(name,fn)};
 const messages:string[]=[];const ctx={hasUI:true,ui:{notify:(value:string)=>messages.push(value)}};
 try {
  await extension(api as any);expect(calls).toBe(1);expect(providers.size).toBe(2);
  const selected=()=>[...providers.entries()].find(([name])=>name.startsWith("eavs-device-"))?.[1];
  expect(selected().models[0].id).toBe("model-a");expect(selected().apiKey).toBe(`\${${keyRef}}`);
  model="model-b";await commands.get("eavs-refresh").handler("",ctx);expect(selected().models[0].id).toBe("model-b");
  deny=true;await commands.get("eavs-refresh").handler("",ctx);expect(selected().models[0].id).toBe("model-b");expect(messages.join(" ")).not.toContain("private-provider-secret");
  deny=false;empty=true;await commands.get("eavs-refresh").handler("",ctx);expect([...providers.keys()]).toEqual(["unrelated"]);
  empty=false;await commands.get("eavs-refresh").handler("",{hasUI:false});expect(providers.size).toBe(2);
  await events.get("session_shutdown")?.({},ctx);expect([...providers.keys()]).toEqual(["unrelated"]);
  const before=calls;await commands.get("eavs-refresh").handler("",ctx);expect(calls).toBe(before);
  expect(await readFile(path,"utf8")).toBe(original);
  const previousOffline=process.env.PI_OFFLINE;
  try {
   process.env.PI_OFFLINE="1";await extension(api as any);expect(calls).toBe(before);expect(providers.size).toBe(1);
   await commands.get("eavs-refresh").handler("",ctx);expect(calls).toBe(before+1);expect(providers.size).toBe(2);
  } finally {if(previousOffline===undefined)delete process.env.PI_OFFLINE;else process.env.PI_OFFLINE=previousOffline;}
 } finally {await events.get("session_shutdown")?.({},ctx);delete process.env[keyRef];await new Promise<void>(resolve=>server.close(()=>resolve()));await rm(directory,{recursive:true,force:true});}
});
