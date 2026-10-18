// Production Node-runtime proof: real HTTP transport, explicit proxy bypass,
// startup/refresh/shutdown and credential-blind profiles. Only synthetic loopback.
import test from "node:test";
import assert from "node:assert/strict";
import {createServer} from "node:http";
import {createServer as createHttpsServer} from "node:https";
import {execFileSync} from "node:child_process";
import {mkdtemp,writeFile,readFile,rm} from "node:fs/promises";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {fetchPiProviders} from "../adapters/pi/provider-library.ts";
import extension from "../adapters/pi/extension.ts";

const catalog=()=>({schema_version:1,scope:"key-filtered-chat-candidates",providers:[{id:"alpha",api:"openai-responses",base_path:"/alpha/v1",request_path:"/alpha/v1/responses",operation:"chat",models:[{id:"model-a",name:"Model A",reasoning:true,input:["text"],context_window:128_000,max_tokens:4096,pricing:{status:"unverified_estimate",unit:"USD_per_million_tokens",rates:{input:1,output:2,cache_read:0.5,cache_write:1.25}}}]}]});

test("Node TLS verification cannot be disabled by ambient environment",async()=>{
 const directory=await mkdtemp(join(tmpdir(),"eavs-node-tls-"));const key=join(directory,"key.pem"),cert=join(directory,"cert.pem");
 const previous=process.env.NODE_TLS_REJECT_UNAUTHORIZED;
 let received=0;let server:ReturnType<typeof createHttpsServer>|undefined;
 try {
  execFileSync("openssl",["req","-x509","-newkey","rsa:2048","-nodes","-days","1","-subj","/CN=localhost","-keyout",key,"-out",cert],{stdio:"ignore",timeout:10_000});
  server=createHttpsServer({key:await readFile(key),cert:await readFile(cert)},(_request,response)=>{received++;response.end(JSON.stringify(catalog()));});
  await new Promise<void>(resolve=>server?.listen(0,"127.0.0.1",resolve));const address=server.address();assert.ok(address && typeof address!=="string");
  process.env.NODE_TLS_REJECT_UNAUTHORIZED="0";
  await assert.rejects(fetchPiProviders({name:"tls",url:`https://127.0.0.1:${address.port}`,key_env:"SYNTHETIC",allow_loopback_http:false},{SYNTHETIC:"eavs-synthetic000000000000000000000000000000"}),/eavs catalog unavailable; no inference was attempted/);
  assert.equal(received,0,"no virtual-key-bearing HTTP request may cross an invalid TLS connection");
 } finally {
  if(previous===undefined)delete process.env.NODE_TLS_REJECT_UNAUTHORIZED;else process.env.NODE_TLS_REJECT_UNAUTHORIZED=previous;
  if(server)await new Promise<void>(resolve=>server?.close(()=>resolve()));await rm(directory,{recursive:true,force:true});
 }
});

test("Node startup discovery bypasses ambient proxies, preserves scopes and cleans lifecycle",async()=>{
 const directory=await mkdtemp(join(tmpdir(),"eavs-node-provider-"));const path=join(directory,"device.json");
 const envKey=`EAVS_NODE_TEST_${crypto.randomUUID().replaceAll("-","")}`;
 const proxy=process.env.HTTP_PROXY,noProxy=process.env.NO_PROXY;
 process.env.HTTP_PROXY="http://127.0.0.1:9";process.env.NO_PROXY="";process.env[envKey]="eavs-synthetic000000000000000000000000000000";
 let mode="good",calls=0,other=0;
 const server=createServer((request,response)=>{
  if(request.url!=="/v1/eavs/catalog"){other++;response.writeHead(404);response.end();return;}
  calls++;
  assert.equal(request.headers.authorization,`Bearer ${process.env[envKey]}`);
  assert.equal(request.headers["x-eavs-token"],"synthetic-listener");
  if(mode==="redirect"){response.writeHead(302,{Location:"/must-not-follow"});response.end();return;}
  if(mode==="denied"){response.writeHead(401);response.end("private-error-must-not-echo");return;}
  if(mode==="large"){response.end("x".repeat(4*1024*1024+1));return;}
  response.end(JSON.stringify(mode==="empty"?{schema_version:1,scope:"key-filtered-chat-candidates",providers:[]}:catalog()));
 });
 await new Promise<void>(resolve=>server.listen(0,"127.0.0.1",resolve));const address=server.address();assert.ok(address && typeof address!=="string");
 const listenerKey=`EAVS_NODE_LISTENER_${crypto.randomUUID().replaceAll("-","")}`;process.env[listenerKey]="synthetic-listener";
 const connection={name:"fixture",url:`http://127.0.0.1:${address.port}`,key_env:envKey,listener_key_env:listenerKey,allow_loopback_http:true};
 const providers=new Map<string,any>([["unrelated",{keep:true}]]),events=new Map<string,Function>(),commands=new Map<string,any>();
 const api={registerFlag:()=>{},getFlag:(name:string)=>name==="eavs-device"?"fixture":path,registerProvider:(name:string,value:any)=>providers.set(name,value),unregisterProvider:(name:string)=>providers.delete(name),registerCommand:(name:string,value:any)=>commands.set(name,value),on:(name:string,fn:Function)=>events.set(name,fn)};
 const messages:string[]=[];const ctx={hasUI:true,ui:{notify:(message:string)=>messages.push(message)}};
 try {
  await writeFile(path,JSON.stringify({connections:[connection]}));
  await extension(api as any);assert.equal(calls,1);assert.equal(providers.size,2);
  const definition=[...providers.entries()].find(([name])=>name.startsWith("eavs-device-"))?.[1];assert.equal(definition.apiKey,`\${${envKey}}`);
  for(const failure of ["redirect","denied","large"]){mode=failure;await assert.rejects(fetchPiProviders(connection),/eavs catalog unavailable; no inference was attempted/);}
  assert.equal(other,0);
  mode="denied";await commands.get("eavs-refresh").handler("",ctx);assert.equal(providers.size,2);assert.ok(!messages.join(" ").includes("private-error-must-not-echo"));
  mode="empty";await commands.get("eavs-refresh").handler("",ctx);assert.deepEqual([...providers.keys()],["unrelated"]);
  mode="good";await commands.get("eavs-refresh").handler("",{hasUI:false});assert.equal(providers.size,2);
  await events.get("session_shutdown")?.({},ctx);assert.deepEqual([...providers.keys()],["unrelated"]);
  const before=calls;await commands.get("eavs-refresh").handler("",ctx);assert.equal(calls,before);
 } finally {
  await events.get("session_shutdown")?.({},ctx);
  delete process.env[envKey];delete process.env[listenerKey];
  if(proxy===undefined)delete process.env.HTTP_PROXY;else process.env.HTTP_PROXY=proxy;
  if(noProxy===undefined)delete process.env.NO_PROXY;else process.env.NO_PROXY=noProxy;
  await new Promise<void>(resolve=>server.close(()=>resolve()));await rm(directory,{recursive:true,force:true});
 }
});
