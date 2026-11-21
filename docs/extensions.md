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

1. Paste the link into the field. Both formats work:
   - `chromewebstore.google.com/detail/<slug>/<id>`
   - `chrome.google.com/webstore/detail/<slug>/<id>`

   Nea shows the row "Install extension from the Chrome Web Store". On a
   store page, the extensions menu also shows "Install extension from this
   page".
2. Nea downloads the CRX3 from `clients2.google.com/service/update2/crx`.
   The `prodversion` is the version of the installed runtime.
3. Nea verifies the package before it writes a file:
   - The format is `Cr24` version 3.
   - All signatures are correct (RSA PKCS#1 and ECDSA P-256 with SHA-256,
     through CNG).
   - At least one developer signature has a key that gives the signed
     `crx_id`.
   - The Web Store signature has a public key hash equal to the Chromium
     `kPublisherKeyHash`.
   - The id is the same as the id in the link.
4. Nea extracts the package to `%APPDATA%\Nea\extensions\<id>`. Nea reads the
   zip file with its own code. It rejects absolute paths, paths with `..`,
   encrypted entries and zip64 entries. Then it calls `AddBrowserExtension`.
5. If the engine started without extension support, Nea registers the
   extension. The extension loads at the next start, and Nea shows a notice.

`extensions.json` in the data folder lists the installed extensions and their
states (enabled, pinned).

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
