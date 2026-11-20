// Runs when each document is created, before the page parses or paints:
// hides what the person chose to hide on this site. An adopted sheet needs
// no <head> to exist yet and survives the page rewriting its own markup.
(() => {
  if (window.top !== window) return;
  const sites = /*SITES*/;
  const host = location.host.toLowerCase().replace(/^www\./, "");
  const selectors = sites[host];
  if (!selectors) return;
  const sheet = new CSSStyleSheet();
  for (const selector of selectors) {
    try {
      sheet.insertRule(selector + "{display:none!important}", sheet.cssRules.length);
    } catch (error) {
      // A selector this engine can't parse hides nothing; the rest still apply.
    }
  }
  document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  window.__livianoCurtainSheet = sheet;
})();
