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
  // The app's own look (design.rs): a pill in ink with the ground's colour
  // for text, rising a little as it fades in and sinking as it goes.
  note.style.cssText = "font:500 13px/1.4 'Segoe UI Variable Text','Segoe UI',system-ui,sans-serif;color:#fff;background:rgba(23,23,23,.94);padding:8px 16px;border-radius:999px;box-shadow:0 8px 24px rgba(0,0,0,.16);max-width:80vw;opacity:0;transform:translateY(6px);transition:opacity .14s ease-out,transform .14s ease-out";
  if (matchMedia("(prefers-color-scheme: dark)").matches) {
    note.style.color = "#171717";
    note.style.background = "rgba(237,237,237,.96)";
  }
  if (matchMedia("(prefers-reduced-motion: reduce)").matches) note.style.transition = "none";
  host.attachShadow({ mode: "closed" }).append(note);
  (document.body || document.documentElement).append(host);
  requestAnimationFrame(() => {
    note.style.opacity = "1";
    note.style.transform = "none";
  });
  setTimeout(() => {
    note.style.opacity = "0";
    note.style.transform = "translateY(6px)";
  }, 2400);
  setTimeout(() => host.remove(), 2600);
};
