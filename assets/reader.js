// Reader mode toggle. The original <html> element is kept aside, untouched,
// and put back on the second press, so the page returns exactly as it was
// (scroll included) without reloading.
(() => {
  /*TOAST*/
  const saved = window.__neaReader;
  if (saved) {
    document.replaceChild(saved.original, document.documentElement);
    window.__neaReader = undefined;
    window.scrollTo(0, saved.scroll);
    return "off";
  }

  /*READABILITY*/

  let article = null;
  try {
    // Readability rewrites the tree it reads: give it a copy.
    article = new Readability(document.cloneNode(true)).parse();
  } catch (error) {
    article = null;
  }
  if (!article || !article.content) {
    neaToast("This page has no article to read");
    return "none";
  }

  const original = document.documentElement;
  const html = document.createElement("html");
  html.lang = article.lang || original.lang || "";
  if (article.dir) html.dir = article.dir;
  const head = document.createElement("head");
  const viewport = document.createElement("meta");
  viewport.name = "viewport";
  viewport.content = "width=device-width, initial-scale=1";
  const title = document.createElement("title");
  title.textContent = article.title || document.title;
  const style = document.createElement("style");
  style.textContent = /*CSS*/;
  head.append(viewport, title, style);

  const body = document.createElement("body");
  const main = document.createElement("main");
  main.className = "nea-reader";
  const header = document.createElement("header");
  if (article.siteName) {
    const site = document.createElement("p");
    site.className = "site";
    site.textContent = article.siteName;
    header.append(site);
  }
  const heading = document.createElement("h1");
  heading.textContent = article.title || document.title;
  header.append(heading);
  if (article.byline) {
    const byline = document.createElement("p");
    byline.className = "byline";
    byline.textContent = article.byline;
    header.append(byline);
  }
  const content = document.createElement("article");
  content.innerHTML = article.content;
  main.append(header, content);
  body.append(main);
  html.append(head, body);

  window.__neaReader = { original, scroll: window.scrollY };
  document.replaceChild(html, original);
  window.scrollTo(0, 0);
  return "on";
})();
