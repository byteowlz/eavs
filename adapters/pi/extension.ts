// Opt-in dynamic eavs providers. No models.json mutation, subprocess or inference.
import type {ExtensionAPI} from "@earendil-works/pi-coding-agent";
import {isAbsolute, join} from "node:path";
import {readDeviceConnection} from "./provider-library.ts";
import {LiveProviders} from "./live-providers.ts";

export default async function(pi: ExtensionAPI) {
 pi.registerFlag("eavs-device", {description:"Explicit trusted eavs device connection to discover",type:"string"});
 pi.registerFlag("eavs-device-config", {description:"Explicit machine-local eavs device registry path",type:"string"});
 const configured = pi.getFlag("eavs-device") ?? process.env.EAVS_DEVICE_NAME;
 const live = new LiveProviders(pi);
 let startupFailed = false;
 const refresh = async () => {
  if (typeof configured !== "string" || !configured) throw new Error("select an explicit eavs device connection");
  const explicit = pi.getFlag("eavs-device-config");
  let path: string;
  if (typeof explicit === "string" && explicit) path = explicit;
  else {
   const xdg = process.env.XDG_CONFIG_HOME;
   const base = xdg && isAbsolute(xdg) ? xdg : process.platform === "win32" ? process.env.APPDATA : process.env.HOME ? join(process.env.HOME,".config") : undefined;
   if (!base) throw new Error("machine-local eavs registry unavailable");
   path = join(base,"eavs/device.json");
  }
  if (!isAbsolute(path)) throw new Error("explicit eavs registry must be absolute");
  const connection = await readDeviceConnection(path,configured);
  return live.refresh(connection);
 };
 pi.registerCommand("eavs-refresh", {description:"Refresh scoped eavs model metadata; no inference or config writes",handler:async(args,ctx) => {
  if (args.trim()) {if (ctx.hasUI) ctx.ui.notify("Select the connection with --eavs-device; refresh accepts no arguments.","warning");return;}
  try {
   const skipped = await refresh();
   if (skipped !== undefined && ctx.hasUI) ctx.ui.notify(`Eavs model metadata refreshed; ${skipped} models omitted for incomplete pricing/capabilities. Estimates are not settlement.`,"info");
  } catch {if (ctx.hasUI) ctx.ui.notify("Eavs catalog unavailable. Last-known model metadata retained; gateway authorization still applies. No inference was retried.","warning");}
 }});
 pi.on("session_start",async(_event,ctx) => {if (startupFailed && ctx.hasUI) ctx.ui.notify("Eavs startup discovery unavailable; check the selected device connection, then /eavs-refresh. No fallback was selected.","warning");});
 pi.on("session_shutdown",async() => {live.close();});
 // Only a bounded HTTPS/explicit-loopback catalog read in the async factory.
 // No processes, watchers, background timers or credential commands are started.
 // Pi awaits this before initial model selection, including --list-models.
 if (configured && process.env.PI_OFFLINE !== "1") {try {await refresh();} catch {startupFailed=true;}}
}
