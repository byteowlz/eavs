// EAVS Console service worker: cache the static shell only. Data is always
// fetched live (it is authenticated and volatile).
const SHELL = ["/admin", "/admin/app", "/admin/manifest.webmanifest", "/admin/icon.svg"];
self.addEventListener("install", (event) => {
  event.waitUntil(caches.open("eavs-console-v1").then((c) => c.addAll(SHELL)).then(() => self.skipWaiting()));
});
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));
self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);
  if (event.request.method !== "GET" || !SHELL.includes(url.pathname)) return;
  event.respondWith(
    caches.match(url.pathname).then((hit) =>
      hit || fetch(event.request).then((res) => {
        const copy = res.clone();
        caches.open("eavs-console-v1").then((c) => c.put(url.pathname, copy));
        return res;
      })
    )
  );
});
