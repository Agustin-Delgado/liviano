// Hides the cookie banners the list knows for this page. Runs once per
// document (top frame only): the first caller leaves its sheet in
// `window.__neaCookieSheet`.
//
// `selectors` are the ones that apply before the page has anything: the
// site's own and the generic rules no id or class can pick. The rest of the
// generic rules (a lone `#id` or `.class`, almost all of them) come from
// `path` for the ids and classes this page really has, as they appear: a
// sheet with all of them cost ~10 MB per tab.
const neaHideCookies = (selectors, path) => {
  if (window.__neaCookieSheet) return;
  const sheet = new CSSStyleSheet();
  // In groups: one rule per selector would cost a rule object each. A
  // group the engine refuses (one bad selector spoils its list) is split.
  const add = (list) => {
    for (let i = 0; i < list.length; i += 64) {
      const group = list.slice(i, i + 64);
      try {
        sheet.insertRule(group.join(",") + "{display:none!important}", sheet.cssRules.length);
      } catch (error) {
        for (const one of group) {
          try {
            sheet.insertRule(one + "{display:none!important}", sheet.cssRules.length);
          } catch (error) {
            // Not CSS this engine knows: hides nothing.
          }
        }
      }
    }
  };
  add(selectors);
  document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  window.__neaCookieSheet = sheet;

  // A banner often locks the page's scroll while it waits for an answer;
  // once it is hidden nobody can answer, so the lock goes too. Only when a
  // hidden banner is really there.
  const unlock = () => {
    const hidden = Array.from(sheet.cssRules).some((rule) => {
      try {
        return document.querySelector(rule.selectorText) !== null;
      } catch (error) {
        return false;
      }
    });
    if (!hidden) return;
    for (const element of [document.documentElement, document.body]) {
      if (!element) continue;
      const style = getComputedStyle(element);
      if (style.overflow === "hidden" || style.overflowY === "hidden") {
        element.style.setProperty("overflow", "auto", "important");
        element.style.setProperty("overflow-y", "auto", "important");
      }
    }
  };

  // Ids and classes go to the app as `i<id>` and `c<class>`, each once.
  // Only plain names: the list's indexed rules use nothing else.
  const sent = new Set();
  let pending = [];
  let asking = false;
  const note = (token) => {
    if (sent.has(token) || !/^[ic][A-Za-z_-][\w-]*$/.test(token)) return;
    sent.add(token);
    pending.push(token);
  };
  const collect = (element) => {
    if (element.id) note("i" + element.id);
    for (const name of element.classList) note("c" + name);
  };
  const scan = (root) => {
    collect(root);
    for (const element of root.querySelectorAll("[id],[class]")) collect(element);
  };
  const answered = (list) => {
    if (Array.isArray(list) && list.length) {
      add(list);
      unlock();
    }
  };
  // Until the first frame the question is synchronous, so a banner that is
  // already in the page's markup is never painted: the observer's callback
  // runs before the engine renders, and the answer comes from this machine.
  // Afterwards it doesn't block the page.
  let painted = false;
  requestAnimationFrame(() => setTimeout(() => (painted = true)));
  // Kept well short of what a URL can carry.
  const url = (batch) => path + "?t=" + batch.join(",");
  const ask = () => {
    while (!painted && pending.length) {
      try {
        const request = new XMLHttpRequest();
        request.open("GET", url(pending.splice(0, 400)), false);
        request.send();
        if (request.status === 200) answered(JSON.parse(request.responseText));
      } catch (error) {
        // Asked again, without waiting, once the page has painted.
        break;
      }
    }
    if (asking || !pending.length) return;
    const batch = pending.splice(0, 400);
    asking = true;
    fetch(url(batch), { cache: "no-store" })
      .then((response) => (response.status === 200 ? response.json() : []))
      .then(answered)
      .catch(() => {})
      .finally(() => {
        asking = false;
        ask();
      });
  };
  const observer = new MutationObserver((records) => {
    for (const record of records) {
      if (record.type === "attributes") collect(record.target);
      else for (const node of record.addedNodes) if (node.nodeType === 1) scan(node);
    }
    ask();
  });
  observer.observe(document, { childList: true, subtree: true, attributes: true, attributeFilter: ["id", "class"] });
  if (document.documentElement) scan(document.documentElement);
  ask();

  // Banners show up while the page settles; after that nothing keeps
  // watching it.
  const settle = () => {
    unlock();
    setTimeout(unlock, 1500);
    setTimeout(() => {
      unlock();
      observer.disconnect();
      sent.clear();
    }, 20000);
  };
  if (document.readyState === "complete") settle();
  else addEventListener("load", settle, { once: true });
};
