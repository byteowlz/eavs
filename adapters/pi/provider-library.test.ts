import {test, expect} from "bun:test";
import {mkdtemp,writeFile,symlink,rm,readFile} from "node:fs/promises";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {createServer} from "node:http";
import {fetchPiProviders,projectPiProviders,readDeviceConnection,type DeviceConnection} from "./provider-library.ts";
import {LiveProviders} from "./live-providers.ts";
import {isolatedProxyEnvironment} from "./test-environment.ts";
isolatedProxyEnvironment();

const connection = (): DeviceConnection => ({name:"central",url:"https://eavs.example.test",key_env:"EAVS_KEY",listener_key_env:"EAVS_LISTENER",allow_loopback_http:false});
const fixture = () => ({schema_version:1,scope:"key-filtered-chat-candidates",providers:[{id:"alpha",api:"anthropic-messages",base_path:"/alpha/v1",request_path:"/alpha/v1/messages",operation:"chat",models:[{id:"model-a",name:"Model A",reasoning:true,input:["text","image"],context_window:200_000,max_tokens:4096,compat:{},pricing:{status:"unverified_estimate",unit:"USD_per_million_tokens",rates:{input:3,output:15,cache_read:0.3,cache_write:3.75}}}]}]});
const synthetic = {EAVS_KEY:"eavs-synthetic000000000000000000000000000000",EAVS_LISTENER:"synthetic-listener"};

test("Rust and TypeScript share the same credential-blind Pi projection",async() => {
 const projected=projectPiProviders(connection(),fixture());
 expect(Object.values(projected.providers)[0].headers).toEqual({Authorization:"Bearer ${EAVS_KEY}","X-Eavs-Token":"${EAVS_LISTENER}"});
 expect(JSON.stringify(projected)).not.toContain(synthetic.EAVS_LISTENER);
 if (process.env.EAVS_PI_CONFIG_FIXTURES) {
  const config=JSON.parse(await readFile(join(process.env.EAVS_PI_CONFIG_FIXTURES,"models.json"),"utf8"));
  const managed=Object.fromEntries(Object.entries(config.providers).filter(([name])=>name.startsWith("eavs-device-")));
  expect(projected.providers).toEqual(managed);
 }
});

test("hostile scope/routes/costs and duplicate models cannot become free providers",()=>{
 for (const mutate of [(v:any)=>v.schema_version=2,(v:any)=>v.scope="all",(v:any)=>v.providers[0].base_path="/admin/keys",(v:any)=>v.providers[0].api="__proto__",(v:any)=>v.providers[0].models.push(v.providers[0].models[0]),(v:any)=>v.providers[0].models[0].pricing.rates.input=0,(v:any)=>v.providers[0].models[0].pricing.rates.output=-1,(v:any)=>v.providers[0].models[0].pricing=null]) {
  const value=fixture();mutate(value);expect(()=>projectPiProviders(connection(),value)).toThrow();
 }
 const codex=fixture();codex.providers[0].api="openai-codex-responses";codex.providers[0].request_path="/alpha/v1/responses";
 expect(Object.values(projectPiProviders(connection(),codex).providers)[0].api).toBe("openai-responses");
 expect(projectPiProviders(connection(),{schema_version:1,scope:"key-filtered-chat-candidates",providers:[]}).providers).toEqual({});
});

test("only an explicit bounded regular device registry is read; commands are rejected",async()=>{
 const directory=await mkdtemp(join(tmpdir(),"eavs-pi-profile-"));const path=join(directory,"device.json");
 try {
  await writeFile(path,JSON.stringify({connections:[connection()]}));
  expect(await readDeviceConnection(path,"central")).toEqual(connection());
  await symlink(path,join(directory,"link.json"));await expect(readDeviceConnection(join(directory,"link.json"),"central")).rejects.toThrow();
  for (const value of [{connections:[{...connection(),key_env:"!private-command"}]},{connections:[{...connection(),url:"https://user:private@example.test"}]},{connections:[connection(),connection()]},{connections:[{...connection(),url:"http://example.test",allow_loopback_http:true}]},{connections:[{...connection(),secret_file:"/must-not-read"}]}]) {
   await writeFile(path,JSON.stringify(value));await expect(readDeviceConnection(path,"central")).rejects.toThrow("eavs device metadata unavailable or invalid");
  }
 } finally {await rm(directory,{recursive:true,force:true});}
});

test("bounded authenticated catalog transport has separate listener auth and never follows redirects",async()=>{
 const proxy=process.env.HTTP_PROXY,noProxy=process.env.NO_PROXY;
 let calls=0,targetCalls=0,mode="good";
 const server=createServer((request,response)=>{
  if(request.url!=="/v1/eavs/catalog"){targetCalls++;response.end("private-target");return;}
  calls++;
  if(mode==="redirect"){response.writeHead(302,{Location:"/private-target"});response.end();return;}
  if(mode==="denied"){response.writeHead(401);response.end("private-credential-error");return;}
  if(mode==="large"){response.writeHead(200);response.end("x".repeat(4*1024*1024+1));return;}
  if(request.headers.authorization!==`Bearer ${synthetic.EAVS_KEY}` || request.headers["x-eavs-token"]!==synthetic.EAVS_LISTENER){response.writeHead(401);response.end();return;}
  response.end(JSON.stringify(fixture()));
 });
 await new Promise<void>(resolve=>server.listen(0,"127.0.0.1",resolve));
 const address=server.address();if(!address || typeof address==="string") throw new Error("fixture address");
 const c={...connection(),url:`http://127.0.0.1:${address.port}`,allow_loopback_http:true};
 try {
  await expect(fetchPiProviders(c,{})).rejects.toThrow();expect(calls).toBe(0);
  process.env.HTTP_PROXY="http://127.0.0.1:9";process.env.NO_PROXY="";
  await expect(fetchPiProviders(c,synthetic)).rejects.toThrow();expect(calls).toBe(0); // affected Bun fails closed, never forwards the key
  delete process.env.HTTP_PROXY;
  const result=await fetchPiProviders(c,synthetic);expect(Object.keys(result.providers)).toHaveLength(1);expect(calls).toBe(1);
  for (const value of ["redirect","denied","large"]) {mode=value;await expect(fetchPiProviders(c,synthetic)).rejects.toThrow("eavs catalog unavailable; no inference was attempted");}
  expect(targetCalls).toBe(0);
  const aborted=new AbortController();aborted.abort();await expect(fetchPiProviders(c,synthetic,aborted.signal)).rejects.toThrow();
 } finally {if(proxy===undefined)delete process.env.HTTP_PROXY;else process.env.HTTP_PROXY=proxy;if(noProxy===undefined)delete process.env.NO_PROXY;else process.env.NO_PROXY=noProxy;await new Promise<void>(resolve=>server.close(()=>resolve()));}
});

test("late metadata cannot replace a newer explicit refresh",async()=>{
 let calls=0;let seen:()=>void=()=>{};const firstSeen=new Promise<void>(resolve=>{seen=resolve;});
 const server=createServer((_request,response)=>{if(++calls===1){seen();return;}const current=fixture();current.providers[0].models[0].id="newer";response.end(JSON.stringify(current));});
 await new Promise<void>(resolve=>server.listen(0,"127.0.0.1",resolve));const address=server.address();if(!address || typeof address==="string")throw new Error("fixture address");
 const c={...connection(),url:`http://127.0.0.1:${address.port}`,allow_loopback_http:true};
 const registered=new Map<string,any>();const live=new LiveProviders({registerProvider:(name,definition)=>{registered.set(name,definition);},unregisterProvider:name=>{registered.delete(name);}});
 try {const older=live.refresh(c,synthetic);await firstSeen;await live.refresh(c,synthetic);expect(await older).toBeUndefined();expect([...registered.values()][0].models[0].id).toBe("newer");}
 finally {live.close();await new Promise<void>(resolve=>server.close(()=>resolve()));}
});

test("live registry refresh only replaces owned providers and respects empty scope/shutdown",async()=>{
 let catalog:any=fixture();let denied=false;
 const server=createServer((_request,response)=>{if(denied){response.writeHead(401);response.end("private-error");return;}response.end(JSON.stringify(catalog));});
 await new Promise<void>(resolve=>server.listen(0,"127.0.0.1",resolve));const address=server.address();if(!address || typeof address==="string") throw new Error("fixture address");
 const c={...connection(),url:`http://127.0.0.1:${address.port}`,allow_loopback_http:true};
 const registrations=new Map<string,any>([["unrelated",{keep:true}]]);
 const live=new LiveProviders({registerProvider:(name,value)=>{registrations.set(name,value);},unregisterProvider:name=>{registrations.delete(name);}});
 try {
  await live.refresh(c,synthetic);expect(registrations.size).toBe(2);
  denied=true;await expect(live.refresh(c,synthetic)).rejects.toThrow("eavs catalog unavailable; no inference was attempted");expect(registrations.size).toBe(2);
  denied=false;catalog={schema_version:1,scope:"key-filtered-chat-candidates",providers:[]};await live.refresh(c,synthetic);expect([...registrations.keys()]).toEqual(["unrelated"]);
  catalog=fixture();await live.refresh(c,synthetic);live.close();live.close();expect([...registrations.keys()]).toEqual(["unrelated"]);
  expect(await live.refresh(c,synthetic)).toBeUndefined();
 } finally {live.close();await new Promise<void>(resolve=>server.close(()=>resolve()));}
});
