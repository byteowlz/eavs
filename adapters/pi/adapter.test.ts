import { describe, expect, test } from "bun:test";
import { fileURLToPath } from "node:url";
const adapter = fileURLToPath(new URL("./adapter.ts", import.meta.url));
async function merge(existing: string, providers: unknown[] = []) {
  const child = Bun.spawn([process.execPath, adapter], { stdin:"pipe", stdout:"pipe", stderr:"pipe" });
  child.stdin.write(JSON.stringify({method:"merge",existing,providers,base_url:"http://127.0.0.1:3033",api_key:"EAVS_API_KEY"}));
  child.stdin.end();
  const [stdout,stderr,exit] = await Promise.all([new Response(child.stdout).text(),new Response(child.stderr).text(),child.exited]);
  return {stdout,stderr,exit};
}
describe("Pi merge never replaces corrupt configuration with a fresh export", () => {
  test("invalid roots fail privately with no replacement document", async () => {
    for (const existing of ["{private-secret", "null", "[]", "42", `{"providers":null}`, `{"providers":[]}`, `{"providers":"private-secret"}`, `{"providers":{"native":null}}`]) {
      const reply = await merge(existing);
      expect(reply.exit).not.toBe(0);
      expect(reply.stdout).toBe("");
      expect(reply.stderr).not.toContain("private-secret");
      expect(JSON.parse(reply.stderr).error).toContain("Cannot merge");
    }
  });
  test("preserves unrelated providers and all unrelated root fields", async () => {
    const unrelated = {models:[{id:"custom"}],apiKey:"local-reference",custom:{nested:[1,2,3]}};
    const input = {providers:{native:unrelated,"eavs-old":{models:[]}},other:{keep:true}};
    const reply = await merge(JSON.stringify(input));
    expect(reply.exit).toBe(0);
    expect(reply.stderr).toBe("");
    expect(JSON.parse(reply.stdout)).toEqual({providers:{native:unrelated},other:{keep:true}});
    expect(input.providers["eavs-old"]).toEqual({models:[]});
  });
  test("absent provider root is valid and new managed providers are merged", async () => {
    const reply = await merge(`{"custom":{"keep":true}}`,[{name:"alpha",type:"openai-compatible",models:[]}]);
    expect(reply.exit).toBe(0);
    const result = JSON.parse(reply.stdout);
    expect(result.custom).toEqual({keep:true});
    expect(result.providers["eavs-alpha"].apiKey).toBe("EAVS_API_KEY");
  });
});
