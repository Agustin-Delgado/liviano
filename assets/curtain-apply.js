// Replaces the hidden set of an already loaded page (after restoring
// something), so the change shows without a reload.
(() => {
  const selectors = /*SELECTORS*/;
  let sheet = window.__neaCurtainSheet;
  if (!sheet) {
    sheet = new CSSStyleSheet();
    document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
    window.__neaCurtainSheet = sheet;
  }
  while (sheet.cssRules.length) sheet.deleteRule(0);
  for (const selector of selectors) {
    try {
      sheet.insertRule(selector + "{display:none!important}", sheet.cssRules.length);
    } catch (error) {
      // Unparseable selectors hide nothing.
    }
  }
  return sheet.cssRules.length;
})();
