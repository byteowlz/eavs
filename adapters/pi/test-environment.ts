import {beforeAll,afterAll} from "bun:test";
// Synthetic loopback tests only. Bun1.3.13 cannot bypass ambient proxies per
// request; production fails closed in that situation and Node is tested separately.
const names=["HTTP_PROXY","http_proxy","HTTPS_PROXY","https_proxy","ALL_PROXY","all_proxy"];
export function isolatedProxyEnvironment(){
 const saved=new Map<string,string|undefined>();
 beforeAll(()=>{for(const name of names){saved.set(name,process.env[name]);delete process.env[name];}});
 afterAll(()=>{for(const [name,value] of saved){if(value===undefined)delete process.env[name];else process.env[name]=value;}});
}
