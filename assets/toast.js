// A short note at the bottom of the page, for the injected features to say
// why nothing happened. Lives outside the page's styles (shadow root).
const livianoToast = (text) => {
  const old = document.getElementById("liviano-toast");
  if (old) old.remove();
  const host = document.createElement("div");
  host.id = "liviano-toast";
  host.style.cssText = "all:initial;position:fixed;z-index:2147483647;left:0;right:0;bottom:24px;display:flex;justify-content:center;pointer-events:none";
  const note = document.createElement("div");
  note.textContent = text;
  note.style.cssText = "font:14px/1.4 'Segoe UI',system-ui,sans-serif;color:#fff;background:rgba(32,32,36,.92);padding:8px 14px;border-radius:8px;box-shadow:0 4px 16px rgba(0,0,0,.25);max-width:80vw";
  host.attachShadow({ mode: "closed" }).append(note);
  (document.body || document.documentElement).append(host);
  setTimeout(() => host.remove(), 2600);
};
