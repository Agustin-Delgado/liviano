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
  // The app's look (design.rs, after celagem-svelte's toast): a pill in ink
  // with the ground's colour for text. It comes up from 6 px below at 0.95
  // on the toast's spring (0.66 of its 600 ms) and leaves the same way in
  // 400 ms; it stays 4 s, as HKUI's toasts do.
  const spring = "linear(0,0.002 0.6%,0.007 1.2%,0.015 1.8%,0.026 2.4%,0.041 3.1%,0.06 3.8%,0.108 5.3%,0.157 6.6%,0.214 8%,0.467 13.7%,0.577 16.3%,0.631 17.7%,0.682 19.1%,0.73 20.5%,0.771 21.8%,0.808 23.1%,0.844 24.5%,0.874 25.8%,0.903 27.2%,0.928 28.6%,0.952 30.1%,0.972 31.6%,0.988 33.1%,1.01 35.7%,1.025 38.5%,1.034 41.6%,1.038 45%,1.035 50.1%,1.012 64.2%,1.003 73%,0.999 83.7%,1)";
  const hidden = "translateY(6px) scale(0.95)";
  note.style.cssText = "font:500 13px/16px 'Segoe UI Variable Text','Segoe UI',system-ui,sans-serif;color:#fafafa;background:#171717;padding:8px 14px;border-radius:999px;box-shadow:inset 0 1px 0 rgba(255,255,255,.14),0 12px 32px -8px rgba(0,0,0,.18),0 0 0 1px rgba(0,0,0,.08);max-width:80vw;opacity:0;transform:" + hidden + ";transition:opacity 396ms " + spring + ",transform 396ms " + spring;
  if (matchMedia("(prefers-color-scheme: dark)").matches) {
    note.style.color = "#171717";
    note.style.background = "#ededed";
  }
  const still = matchMedia("(prefers-reduced-motion: reduce)").matches;
  if (still) note.style.transition = "none";
  host.attachShadow({ mode: "closed" }).append(note);
  (document.body || document.documentElement).append(host);
  requestAnimationFrame(() => {
    note.style.opacity = "1";
    note.style.transform = "none";
  });
  setTimeout(() => {
    if (!still) note.style.transition = "opacity 400ms ease, transform 400ms ease";
    note.style.opacity = "0";
    note.style.transform = hidden;
  }, 4000);
  setTimeout(() => host.remove(), 4400);
};
