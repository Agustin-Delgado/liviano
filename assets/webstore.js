// The Chrome Web Store page installs with `chrome.webstorePrivate` and reads
// what is installed with `chrome.management`. In WebView2 the engine shows
// its own dialog ("Add to Microsoft Edge") and then its download fails
// ("Download interrupted"): the engine has no store download address. This
// script gives these calls to Nea:
// - an install: Nea downloads, verifies and asks the person, then answers
//   through `__neaWebstoreAnswer`;
// - the installed list and the state of the button: the store extensions
//   of Nea (`__neaWebstoreList`) are added to what the engine reports;
// - "Remove from Chrome": Nea asks the person and answers through
//   `__neaWebstoreRemoved`.
(() => {
  if (location.origin !== "https://chromewebstore.google.com" || window.top !== window) return;
  const chrome = window.chrome;
  const store = chrome && chrome.webstorePrivate;
  const management = chrome && chrome.management;
  if (!store || !management || !chrome.webview || store.__nea) return;
  const post = (kind, id) => chrome.webview.postMessage({ nea: "webstore", kind, id: id || "" });

  // The store calls the API with a callback or as a promise: both work. A
  // failure gives the callback `chrome.runtime.lastError`, as the engine does.
  const reply = (args, value) => {
    const callback = typeof args[args.length - 1] === "function" ? args[args.length - 1] : null;
    if (!callback) return value;
    value.then(
      (v) => callback(v),
      (error) => {
        const runtime = chrome.runtime;
        try {
          Object.defineProperty(runtime, "lastError", { value: { message: String(error.message || error) }, configurable: true });
          callback();
        } finally {
          delete runtime.lastError;
        }
      },
    );
    return undefined;
  };

  // The store extensions of Nea: id -> the `ExtensionInfo` that the store reads.
  const installed = new Map();
  const info = (id, name, version, enabled) => ({
    id, name, shortName: name, description: "", version, enabled, mayDisable: true,
    type: "extension", installType: "normal", isApp: false, offlineEnabled: false,
    optionsUrl: "", permissions: [], hostPermissions: [], icons: [], homepageUrl: "",
    updateUrl: "https://clients2.google.com/service/update2/crx",
  });
  let listed;
  const list = new Promise((resolve) => {
    listed = resolve;
    // An app that does not answer does not stop the page.
    setTimeout(resolve, 1500);
  });
  window.__neaWebstoreList = (items) => {
    for (const e of items || []) installed.set(e.id, info(e.id, e.name, e.version, e.enabled));
    listed();
  };
  post("list");

  // The page listens to installs and removals: Nea tells these listeners.
  const listeners = { onInstalled: [], onUninstalled: [] };
  for (const name of Object.keys(listeners)) {
    const event = management[name];
    if (!event || !event.addListener) continue;
    const add = event.addListener.bind(event);
    event.addListener = (listener) => {
      listeners[name].push(listener);
      return add(listener);
    };
  }
  const tell = (name, value) => {
    for (const listener of listeners[name]) {
      try {
        listener(value);
      } catch (_) {
        // A listener of the page fails: the others still hear.
      }
    }
  };

  const waiting = new Map();
  const ask = (kind, id) =>
    new Promise((resolve) => {
      const key = kind + ":" + id;
      const old = waiting.get(key);
      if (old) old(null);
      waiting.set(key, resolve);
      post(kind, id);
    });
  const answer = (kind, id, value) => {
    const done = waiting.get(kind + ":" + id);
    waiting.delete(kind + ":" + id);
    if (done) done(value);
  };
  // Nea answers with the result code of `beginInstallWithManifest3`: ""
  // when the extension is installed, else "user_cancelled" or "unknown_error".
  window.__neaWebstoreAnswer = (id, result, name, version) => {
    if (result === "") {
      installed.set(id, info(id, name || "", version || "", true));
      setTimeout(() => tell("onInstalled", installed.get(id)), 0);
    }
    answer("install", id, result);
  };
  window.__neaWebstoreRemoved = (id, removed) => {
    if (removed) {
      installed.delete(id);
      setTimeout(() => tell("onUninstalled", id), 0);
    }
    answer("remove", id, removed);
  };

  const original = {
    getAll: management.getAll.bind(management),
    get: management.get.bind(management),
    uninstall: management.uninstall.bind(management),
    status: store.getExtensionStatus.bind(store),
  };
  const engine = (call, ...args) => new Promise((resolve, reject) => {
    try {
      const value = call(...args, (result) => {
        const error = chrome.runtime.lastError;
        if (error) reject(new Error(error.message));
        else resolve(result);
      });
      if (value && value.then) value.then(resolve, reject);
    } catch (error) {
      reject(error);
    }
  });

  const replace = (object, name, value) =>
    Object.defineProperty(object, name, { value, configurable: true, writable: true });
  try {
    replace(store, "beginInstallWithManifest3", (...args) => {
      const id = String((args[0] && args[0].id) || "");
      if (!/^[a-p]{32}$/.test(id)) return reply(args, Promise.resolve("invalid_id"));
      return reply(args, ask("install", id).then((result) => (result === null ? "user_cancelled" : result)));
    });
    // Nea installs the extension before it answers the first call. Thus the
    // second call has nothing to do.
    for (const name of ["completeInstall", "completeInstallWithCV"]) {
      if (name in store) replace(store, name, (...args) => reply(args, Promise.resolve(undefined)));
    }
    replace(store, "getExtensionStatus", (...args) => {
      const known = installed.get(String(args[0]));
      if (known) return reply(args, Promise.resolve(known.enabled ? "enabled" : "disabled"));
      return reply(args, engine(original.status, ...args.filter((a) => typeof a !== "function")));
    });
    replace(management, "getAll", (...args) =>
      reply(args, list.then(() => engine(original.getAll).catch(() => [])).then((items) => {
        const ids = new Set(items.map((e) => e.id));
        return items.concat([...installed.values()].filter((e) => !ids.has(e.id)));
      })),
    );
    replace(management, "get", (...args) => {
      const id = String(args[0]);
      return reply(args, list.then(() => installed.get(id) || engine(original.get, id)));
    });
    replace(management, "uninstall", (...args) => {
      const id = String(args[0]);
      if (!installed.has(id)) return reply(args, engine(original.uninstall, ...args.filter((a) => typeof a !== "function")));
      return reply(args, ask("remove", id).then((removed) => {
        if (!removed) throw new Error("User cancelled uninstall of extension " + id + ".");
      }));
    });
    Object.defineProperty(store, "__nea", { value: true });
  } catch (_) {
    // The engine changed the objects. The engine's own install stays.
  }
})();
