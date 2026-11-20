// Element picker: the element under the pointer is outlined, a click hides
// it (and tells the app, which stores the selector for this site), the
// arrows walk to the container and back, Esc leaves. Running it again while
// it is active leaves too.
(() => {
  if (window.__livianoPick) {
    window.__livianoPick.stop();
    return "off";
  }

  // Generated names (CSS modules, styled-components, hashes) change with the
  // next deploy of the site; a selector built on them would stop matching.
  const stable = (token) =>
    !!token &&
    token.length <= 48 &&
    !/\d.*\d/.test(token) &&
    !/^(css|sc|jsx|tw|emotion|styled|svelte|astro|ng)-/i.test(token) &&
    !/^_|__[a-z0-9]{4,}$/i.test(token) &&
    /^[a-z_-][a-z0-9_-]*$/i.test(token);

  const unique = (selector) => {
    try {
      return document.querySelectorAll(selector).length === 1;
    } catch (error) {
      return false;
    }
  };

  const step = (el) => {
    if (el.id && stable(el.id) && unique("#" + CSS.escape(el.id))) {
      return { part: "#" + CSS.escape(el.id), anchored: true };
    }
    let part = el.localName;
    const classes = [...el.classList].filter(stable).slice(0, 3);
    part += classes.map((c) => "." + CSS.escape(c)).join("");
    const parent = el.parentElement;
    if (parent) {
      const matching = [...parent.children].filter((c) => c.matches(part));
      if (matching.length > 1) {
        const sameTag = [...parent.children].filter((c) => c.localName === el.localName);
        part += ":nth-of-type(" + (sameTag.indexOf(el) + 1) + ")";
      }
    }
    return { part, anchored: false };
  };

  // The shortest chain of steps, from the element up, that matches only it.
  const selectorFor = (el) => {
    const parts = [];
    for (let node = el; node && node !== document.documentElement; node = node.parentElement) {
      const { part, anchored } = step(node);
      parts.unshift(part);
      const selector = parts.join(" > ");
      if (anchored || unique(selector) || node === document.body) return selector;
    }
    return parts.join(" > ");
  };

  const describe = (el) => {
    let name = el.localName;
    const named = [...el.classList].find(stable);
    if (el.id && stable(el.id)) name += "#" + el.id;
    else if (named) name += "." + named;
    const text = (el.innerText || el.getAttribute("aria-label") || el.getAttribute("alt") || "")
      .replace(/\s+/g, " ")
      .trim();
    if (!text) return name;
    return name + " — " + (text.length > 48 ? text.slice(0, 47) + "…" : text);
  };

  const sheet = () => {
    let current = window.__livianoCurtainSheet;
    if (!current) {
      current = new CSSStyleSheet();
      document.adoptedStyleSheets = [...document.adoptedStyleSheets, current];
      window.__livianoCurtainSheet = current;
    }
    return current;
  };

  const host = document.createElement("div");
  host.style.cssText = "all:initial;position:fixed;inset:0;z-index:2147483647;pointer-events:none";
  const root = host.attachShadow({ mode: "closed" });
  const box = document.createElement("div");
  box.style.cssText =
    "position:fixed;display:none;box-sizing:border-box;background:rgba(214,51,51,.16);border:2px solid #d63333;border-radius:3px;pointer-events:none";
  const tip = document.createElement("div");
  tip.textContent = "Clic: ocultar  ·  ↑ ↓: elegir el contenedor  ·  Esc: salir";
  tip.style.cssText =
    "position:fixed;left:50%;top:12px;transform:translateX(-50%);font:13px/1.4 'Segoe UI',system-ui,sans-serif;color:#fff;background:rgba(32,32,36,.92);padding:7px 14px;border-radius:8px;box-shadow:0 4px 16px rgba(0,0,0,.25);white-space:nowrap;pointer-events:none";
  root.append(box, tip);
  document.documentElement.append(host);

  let target = null;
  const trail = [];

  const pickable = (el) =>
    el && el.nodeType === 1 && el !== host && el !== document.documentElement && el !== document.body;

  const draw = () => {
    if (!target || !target.isConnected) {
      box.style.display = "none";
      return;
    }
    const r = target.getBoundingClientRect();
    box.style.display = "block";
    box.style.left = r.left + "px";
    box.style.top = r.top + "px";
    box.style.width = r.width + "px";
    box.style.height = r.height + "px";
  };

  const block = (event) => {
    event.preventDefault();
    event.stopImmediatePropagation();
  };

  const onMove = (event) => {
    const el = event.target;
    if (!pickable(el) || el === target) return;
    trail.length = 0;
    target = el;
    draw();
  };

  const onClick = (event) => {
    block(event);
    const el = target || event.target;
    if (!pickable(el)) return;
    const selector = selectorFor(el);
    try {
      sheet().insertRule(selector + "{display:none!important}", sheet().cssRules.length);
    } catch (error) {
      return;
    }
    window.chrome.webview.postMessage({ kind: "curtain.hide", selector, label: describe(el) });
    target = null;
    trail.length = 0;
    draw();
  };

  const onKey = (event) => {
    if (event.key === "Escape") {
      block(event);
      stop();
    } else if (event.key === "ArrowUp" && target) {
      block(event);
      const parent = target.parentElement;
      if (pickable(parent)) {
        trail.push(target);
        target = parent;
        draw();
      }
    } else if (event.key === "ArrowDown" && trail.length) {
      block(event);
      target = trail.pop();
      draw();
    }
  };

  const swallowed = ["mousedown", "mouseup", "pointerdown", "pointerup", "dblclick", "auxclick", "touchstart", "touchend"];
  const listen = (on) => {
    const method = on ? "addEventListener" : "removeEventListener";
    window[method]("mouseover", onMove, true);
    window[method]("click", onClick, true);
    window[method]("keydown", onKey, true);
    window[method]("scroll", draw, true);
    window[method]("resize", draw, true);
    for (const type of swallowed) window[method](type, block, true);
  };

  const stop = () => {
    listen(false);
    host.remove();
    window.__livianoPick = undefined;
    window.chrome.webview.postMessage({ kind: "curtain.done" });
  };

  listen(true);
  window.__livianoPick = { stop };
  return "on";
})();
