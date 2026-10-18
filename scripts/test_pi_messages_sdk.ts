// Offline conformance using an explicitly supplied Pi source checkout.
// No installs, credentials or network. Native matrix also checks replay serialization.
import { readFileSync, readdirSync, statSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { resolve, join } from "node:path";
const [source, fixture] = process.argv.slice(2);
if (!source || !fixture) throw new Error("Usage: bun scripts/test_pi_messages_sdk.ts PI_SOURCE_ROOT WIRE_SSE_OR_DIRECTORY");
const api = await import(pathToFileURL(resolve(source, "packages/ai/src/api/pi-messages.ts")).href);
const matrix = statSync(fixture).isDirectory();
const files = matrix ? readdirSync(fixture).filter(file => file.endsWith(".sse")).map(file => join(fixture,file)) : [fixture];
if (!files.length) throw new Error("No fixtures");
for (const file of files) {
  const wire = readFileSync(file);
  let requests = 0;
  const model = { id:"gpt-4o", name:"Fixture", api:"pi-messages", provider:"eavs-fixture", baseUrl:"http://fixture.invalid/alpha/v1/pi", reasoning:true, input:["text"], cost:{input:0,output:0,cacheRead:0,cacheWrite:0}, contextWindow:128000, maxTokens:100 };
  const context = { messages:[{role:"user",content:[{type:"text",text:"Hello"}],timestamp:0}] };
  const execute = (messages: unknown[]) => api.streamSimple(model, {messages}, {
    apiKey:"synthetic-fixture-key",
    fetch:async (url:URL, init:RequestInit) => {
      requests++;
      if (String(url) !== "http://fixture.invalid/alpha/v1/pi/messages" || init.method !== "POST") throw new Error("Wrong Pi endpoint/method");
      const body = JSON.parse(String(init.body));
      if (body.model !== "gpt-4o" || !body.context.messages) throw new Error("Wrong Pi request envelope");
      if (requests === 2) {
        const assistant = body.context.messages.find((message: any) => message.role === "assistant");
        if (!assistant || !assistant.content.some((block: any) => block.type === "toolCall")) throw new Error("Replay lost tool call");
        if (matrix && !assistant.content.some((block: any) => block.type === "thinking")) throw new Error("Replay lost thinking");
        // SDK serialization must retain the opaque provenance envelopes verbatim.
        for (const block of messages.find((message:any) => message.role === "assistant").content) {
          const replay = assistant.content.find((candidate:any) => candidate.type === block.type);
          for (const key of ["thinkingSignature","textSignature","thoughtSignature","redacted"]) {
            if (block[key] !== undefined && replay[key] !== block[key]) throw new Error(`Replay lost ${key}`);
          }
        }
      }
      let offset = 0;
      return new Response(new ReadableStream({pull(controller) {
        if (offset === wire.length) {controller.close(); return;}
        controller.enqueue(new Uint8Array(wire.subarray(offset,++offset)));
      }}), {headers:{"content-type":"text/event-stream"}});
    }
  }).result();
  const result = await execute(context.messages);
  if (result.stopReason !== "toolUse" || result.responseId !== "response-α") throw new Error(`${file}: terminal mismatch: ${JSON.stringify(result)}`);
  const text = result.content.find((block:any) => block.type === "text");
  const call = result.content.find((block:any) => block.type === "toolCall");
  if (text?.text !== "Hello α 🦉" || call?.arguments.path !== "α.txt") throw new Error("Unicode/tool mismatch");
  const usage = result.usage;
  const expectedRead = matrix ? 2 : 0;
  const expectedWrite = file.endsWith("anthropic.sse") ? 1 : 0;
  if (usage.input !== 3 || usage.output !== 2 || usage.cacheRead !== expectedRead || usage.cacheWrite !== expectedWrite || usage.totalTokens !== 5+expectedRead+expectedWrite || !(usage.cost.total > 0)) throw new Error("Usage/cost mismatch");
  await execute([...context.messages,result,{role:"toolResult",toolCallId:call.id,toolName:call.name,content:[{type:"text",text:"ok"}],isError:false,timestamp:0}]);
  if (requests !== 2) throw new Error("Unexpected requests");
  console.log(`Actual Pi decoder/replay accepted ${file} (byte-fragmented, injected fetch, no network)`);
}
