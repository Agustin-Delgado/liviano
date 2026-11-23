# Extensions

Nea loads Chrome extensions through WebView2. It uses
`AreBrowserExtensionsEnabled` in the environment and `AddBrowserExtension` /
`GetBrowserExtensions` in the profile. This page records the tests on runtime
**157.0.4341.60**: what works and what does not. It is the list of known
limitations.

## Memory cost

Nea sets extension support **when it creates the engine**. Thus Nea turns on
extension support only if at least one extension is installed and enabled.
With no extensions, the engine starts without that flag and without extra
processes.

Nea cannot change this while the engine runs. If you install the first
extension, or disable the last one, the change applies at the next start. The
menu and Settings tell you this.

The table shows the private memory of the full process tree, with the same
tab (example.org):

| Engine | Processes | Total private | Without GPU |
|---|---|---|---|
| No extensions | 7 | 166.9 MB | 90.5 MB |
| Test extension enabled | 8 | 177.1 MB | 100.4 MB |

- Each extension with a service worker adds its own renderer process. For a
  minimal extension, this process uses about **20 MB**.
- A heavy extension, for example a blocker with lists, uses more.
- The GPU memory changes by ±10 MB between runs. The extensions do not
  change it.

Thus the last line of the menu is "Each enabled extension adds processes and
memory". Settings shows the same warning.

## Tested APIs

The test extension is in the private test repository (`tests/tools/extension-probe`): MV3, with a service worker,
a content script, a popup and an options page. It calls each API from the
service worker and shows the report on the page.

| API | Result |
|---|---|
| `storage.local`, `storage.sync` | Works |
| `runtime` (messages content ↔ SW, popup ↔ SW) | Works |
| `tabs.query` | Works |
| `tabs.create` | Opens the tab. Nea receives it as `NewWindowRequested` and opens it as a Nea tab. The promise returns `undefined`. |
| `windows.getAll` | Works |
| `bookmarks`, `history`, `downloads` | They respond with the data of **the engine**, not of Nea. Nea keeps its own bookmarks, history and downloads. Thus the extension sees empty or different lists. |
| `sidePanel.getOptions` | Responds, but there is no side panel to show it |
| `sidePanel.open` | Fails: it needs a user gesture and there is no panel |
| `identity.getRedirectURL` | Works |
| `identity.getAuthToken` | Not supported ("not supported on Microsoft Edge") |
| `identity.launchWebAuthFlow` | The non-interactive call fails ("Authorization page could not be loaded"). The interactive call is not verified. |
| `nativeMessaging` | Works. Register the host in `HKCU\Software\Microsoft\Edge\NativeMessagingHosts\<name>` or in `HKCU\Software\Google\Chrome\NativeMessagingHosts\<name>`. |
| `action.setBadgeText`, `action.openPopup` | They exist, but there is no toolbar. The badge is not visible and the popup does not open. |
| `contextMenus`, `alarms`, `scripting.executeScript`, `notifications`, `cookies`, `webRequest`, `declarativeNetRequest`, `i18n`, `permissions`, `offscreen` | Work |
| Go to `chrome-extension://<id>/popup.html` or `options.html` from Nea | Works |

## Limitations and solutions

- **The popup, the options and the side panel open in a tab.**
  - WebView2 has no toolbar and no popup bubble.
  - The puzzle piece menu has these items for each extension: "Open pop-up",
    "Options" and "Side panel". Each item opens a normal tab.
  - Pinned extensions show their initial next to the button. Click the
    initial to open the popup.
  - The puzzle piece button is always next to the gear. With no extension
    installed, its menu shows "No extensions installed", "Get extensions
    from the Chrome Web Store…" (it opens the store in a new tab) and
    "Manage extensions…" (Settings).
  - Result: the popup **sees itself as the active tab**. A popup that acts on
    "the current tab" reports that the tab is not a website. uBlock Origin
    Lite is an example.
- **No badge or counter.** Nea does not show what the extension draws with
  `action`.
- **Browser data.** Bookmark, history and download extensions use the store of
  the engine, not the store of Nea.
- **Reload.**
  - Nea calls `AddBrowserExtension` again with the same folder. The engine
    reloads the extension in place, with the same id. It keeps its `storage`
    and loads the new version (tested from 1.0 to 1.1).
  - A remove and add also reloads, but deletes the `storage`. Thus Nea does
    not use it.
  - During the reload, Chromium sends the extension tabs to
    `chrome-search://local-ntp/local-ntp.html`. In WebView2, this address
    gives `ERR_INVALID_URL`. Thus Nea records these tabs before the reload and
    opens their addresses again after it.
- **Disable** closes the `chrome-extension://` tabs of that extension and
  stops its service worker.
- **Ids.**
  - An unpacked extension gets an id from its folder.
  - For Web Store extensions, Nea writes the public key of the package
    (`key`) into the unpacked `manifest.json`. Thus the engine uses the store
    id, and the pages and the `storage` agree with Chrome.
- **Built-in engine extensions.** WebView2 includes the Microsoft Clipboard
  Extension and the PDF viewer. Nea never changes them. Nea removes from the
  engine only the ids that it registered and that the person removed.

## Install from the Chrome Web Store

### The "Add to Chrome" button

The store page calls `chrome.webstorePrivate.beginInstallWithManifest3` and
then `completeInstall`. WebView2 has this API, but its install cannot work:

- The engine shows its own dialog: "Add … to Microsoft Edge?".
- Then it downloads the package from
  `https://permanently-removed.invalid/?…&prod=chromiumcrx&…&x=id%3D<id>…`.
  WebView2 removes the address of the store update service. Thus the
  download always stops, and the page shows "Download error: Download
  interrupted" (runtime 157.0.4341.60).

Nea changes this. In each document, Nea runs `assets/webstore.js`. On
`https://chromewebstore.google.com` (top frame only), the script replaces
these calls:

| Call of the page | What Nea does |
|---|---|
| `webstorePrivate.beginInstallWithManifest3` | Nea installs the extension (refer to the steps below). Then the page gets `""` (installed), `"user_cancelled"` or `"unknown_error"`. The engine dialog does not open. |
| `webstorePrivate.completeInstall` | Nothing: the extension is already installed. |
| `webstorePrivate.getExtensionStatus` | `"enabled"` or `"disabled"` for a store extension of Nea, else the answer of the engine. |
| `management.getAll`, `management.get` | The answer of the engine, plus the store extensions of Nea. Thus the page shows "Remove from Chrome" also when the engine started without extension support. |
| `management.uninstall` | For a store extension of Nea: Nea asks the person ("Remove '<name>'?"), then removes it. If the person says no, the page gets the error "User cancelled uninstall". |

After an install or a removal, the script also tells the `onInstalled` and
`onUninstalled` listeners of the page. Without this, the store button shows
a spinner and does not stop.

The script sends `{nea: "webstore", kind, id}` messages (`list`, `install`,
`remove`). Nea accepts them only when the engine gives the store origin as
the source.

If the engine starts its own package download anyway, Nea cancels it
(`DownloadStarting`) and installs the extension itself. The download does
not go into the downloads list.

### Other ways to start an install

- Paste the link into the field. Both formats work:
  - `chromewebstore.google.com/detail/<slug>/<id>`
  - `chrome.google.com/webstore/detail/<slug>/<id>`

  Nea shows the row "Install extension from the Chrome Web Store".
- On a store page, the extensions menu shows "Install extension from this
  page".

### The steps

All the slow steps run on a worker thread. The UI thread does not wait.

1. Nea downloads the CRX3 from `clients2.google.com/service/update2/crx`.
   The `prodversion` is the version of the installed runtime.
   - Nea follows each redirect itself (https only, 6 at most).
   - First, Nea downloads without cookies. If the store refuses (401, 403
     or 404), Nea tries again with the cookies of the engine profile for
     `clients2.google.com`, as Chromium does. Some extensions are only for
     persons who are signed in to the store. The cookies go only to
     `clients2.google.com`, never to the host of the package
     (`clients2.googleusercontent.com`).
2. Nea verifies the package before it writes a file:
   - The format is `Cr24` version 3.
   - All signatures are correct (RSA PKCS#1 and ECDSA P-256 with SHA-256,
     through CNG).
   - At least one developer signature has a key that gives the signed
     `crx_id`.
   - The Web Store signature has a public key hash equal to the Chromium
     `kPublisherKeyHash`.
   - The id is the same as the id in the request.
3. Nea extracts the package to `%APPDATA%\Nea\extensions\<id>.new`. Nea
   reads the zip file with its own code. It rejects:
   - absolute paths, drives and paths with `..`,
   - names that Windows changes: device names (`nul`, `com1`…), the
     characters `<>:"|?*`, control characters, and a dot or a space at the
     end of a name,
   - encrypted entries and zip64,
   - more than 100 000 entries or more than 1 GB in total, and packages
     larger than 256 MB.
4. Nea asks the person: "Add the extension '<name>'?". The question lists
   the access that the manifest asks for, in the words that Chrome uses
   ("Read and change all your data on all websites"…). Nea reads this from
   the verified package, not from the page.
5. Yes: Nea moves `<id>.new` to `<id>` (an update replaces the old version
   only now) and calls `AddBrowserExtension`. No: Nea deletes `<id>.new`.
6. If the engine started without extension support, Nea registers the
   extension. The extension loads at the next start, and Nea shows a notice.

`extensions.json` in the data folder lists the installed extensions and their
states (enabled, pinned). At start, Nea deletes the folders in `extensions`
that no extension uses (for example a `<id>.new` that waited when Nea
closed).

### Errors

Nea says why an install did not occur, on the page and in the extensions
menu:

| Cause | Message |
|---|---|
| The store answers 204 or 404 | The store does not give this extension to Nea: it is not available for this browser, for your country, or for a person who is not signed in to the store. |
| The store answers 401 or 403 | Sign in to the store in Nea, then try again. |
| A bad signature, a different id, a damaged zip | The cause, for example "the package signature is not valid". |

### Tested store extensions

On runtime 157.0.4341.60, with a new data folder:

| Extension | Result |
|---|---|
| JSON Formatter (`bcjindcccaagfpapjjmafapmmgkkhgoa`) | Installs from "Add to Chrome" after the question of Nea. Loads after the restart and formats JSON pages. "Remove from Chrome" on the store page removes it from Nea and from the engine. |
| Claude (`fcoeoabgfenejglbffodgkkbkcdhlgfn`) | Without a sign-in, the store page says "This item is not available" and the update service answers 404. Nea shows the reason. Not tested with a signed-in account. Its side panel cannot show in WebView2: Nea opens the side panel page in a tab. |

## For developers

- **Load unpacked.** The menu and Settings have the item "Load unpacked
  extension…". It opens a folder picker (`IFileOpenDialog` with
  `FOS_PICKFOLDERS`). Nea does not copy the folder. It loads the extension
  from its current location.
- **Reload from folder.** This item is also in the menu and in Settings. It
  reads `manifest.json` again and reloads the extension. The `storage` stays.
- **Run the API test again:**

  ```
  cargo run --release --manifest-path tests/probes/Cargo.toml --bin extension_probe -- tests/tools/extension-probe 9335
  node tests/tools/probe-extension.mjs 9335
  ```

  - `PROBE_DATA=<folder>` keeps the data folder between runs.
  - `PROBE_SKIP_ADD=1` starts without a new add of the extension.
