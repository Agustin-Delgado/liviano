// Registered once per view, run as each document is created, before the
// page parses or paints. Asks the app, synchronously, for the cookie-banner
// selectors of this host: the request never leaves the machine (the app
// answers it from its request filter), and its path carries a token made at
// start, so a page can't probe for it. A page whose policy forbids the
// request gets the same selectors a moment later, when its content starts
// loading.
(() => {
  if (window.top !== window || !/^https?:$/.test(location.protocol)) return;
  let selectors;
  try {
    const request = new XMLHttpRequest();
    request.open("GET", "/*PATH*/", false);
    request.send();
    if (request.status !== 200) return; // 204: nothing to hide on this site
    selectors = JSON.parse(request.responseText);
  } catch (error) {
    return;
  }
  if (!Array.isArray(selectors)) return;
  /*HIDE*/
  neaHideCookies(selectors, "/*PATH*/");
})();
