# Sign-in with Google and other accounts

Sign in to Google one time in Nea. Then Gmail, YouTube, Drive, Calendar and
the "Sign in with Google" buttons of other sites use the same session. You do
not sign in again after a restart or an update.

## What works

| Item | Status |
|---|---|
| The Google session stays after a restart, an update, a tab discard, a suspend and an engine restart | Verified with a persistent test cookie on a local page and on `.google.com`. Google keeps its session in persistent cookies. The cookies are in the engine profile, `%LOCALAPPDATA%\Nea\engine`. This folder stays the same in each version. Nea clears no cookies when it closes. |
| One Google session for all Google services | Each Google service reads the same `accounts.google.com` cookies. |
| Sign-in pop-ups ("Sign in with Google", Microsoft, Apple, GitHub and others) | Verified with local pages and with the Google button of a public site. The pop-up opens as a tab next to the page. `window.open` returns the window, the pop-up has its `window.opener`, it sends the result with `postMessage` and it closes itself. Then the page that opened it comes back to the front. |
| The Google button and One Tap (iframes of `accounts.google.com`) on other sites | Verified on a public site with the strict tracking prevention of Nea: the iframe loads, and it can read its cookies and its storage (`document.hasStorageAccess()` is `true`). The ad blocker stops only `accounts.google.com/gsi/log` (EasyPrivacy), which is telemetry. The cookie banner list does not hide One Tap. |
| The Google sign-in page | Verified: `accounts.google.com/ServiceLogin` shows the normal "Sign in" page. |

### How the pop-up works

When a page opens a new window, the engine waits until the view of the new
tab is ready. Then Nea gives that view to the page as its new window. Thus
the new page has the opener and the address loads in it, as in Chrome.

- A page can close its tab with `window.close()` only if a page opened it. A
  tab that you opened stays.
- While a sign-in pop-up (a window of a given size) is open, the page that
  opened it is not suspended or discarded. Thus the result arrives
  immediately.
- The pop-up blocker continues to apply. A blocked pop-up gives `null` to the
  page, and no tab opens.

## Changes in Nea for sign-in

- Before this change, Nea opened each new window as a tab that had no link
  to the page. `window.open` returned `null`, and the pop-up had no
  `window.opener`. Thus "Sign in with Google" in a pop-up could not give the
  result to the site, and the pop-up stayed open. Nea now gives the new tab to
  the page (refer to `src/app/popups_ui.rs`).
- Nea turns off FedCM (`navigator.credentials.get({ identity })`) with
  `--disable-features=FedCm`. WebView2 shows no account chooser for FedCM: a
  request from a local test identity provider stayed open with no end. With
  FedCM off, the site gets `NotSupportedError` immediately and uses its
  pop-up or its iframes. Google's script did not use FedCM in Nea before this
  change (`is_fedcm_supported=false`), so Google sign-in does not change.
- Nea keeps the strict tracking prevention for all sites. It has no
  exception for Google, because the measurements show that it is not
  necessary.

## What Nea cannot do

- **Chrome Sync.** Nea cannot sync bookmarks, passwords, history or open tabs
  with a Google account. Chrome Sync uses a private Google API that only
  Chrome can use.
- **Import of the Chrome session.** Nea cannot import the cookies of Chrome,
  so you must sign in to Google one time in Nea. Chrome encrypts its cookies
  with a key that only Chrome can use (app-bound encryption).
- **The check for "This browser or app may not be secure".** Google can show
  this message after you type the email. Nea is not an embedded web view of
  an app: it is a browser with the Edge engine (WebView2). Its user agent is
  the user agent of Edge, and its client hints also include the brand
  `Microsoft Edge WebView2`. Nea does not change them, because Google asks
  browsers not to use the user agent of a different browser. The tests did
  not sign in to an account, so they did not see the step after the email. If
  Google shows the message, sign in with a passkey or with "Sign in with
  Google" on a site, or report it.
- **Session cookies after the engine stops.** The engine stops when no tab
  has a page (for example, after you close the last tab) and when Nea
  closes. Then the cookies that have no expiry date (session cookies) go
  away, as in Chrome when it closes. Google keeps its session in persistent
  cookies, so the Google session stays.

## What Nea does instead

- **Passwords:** Nea saves them in the Windows Credential Manager. It can
  import them from Chrome, Edge, Brave, Vivaldi, Opera, Arc or a CSV file,
  on the local computer (Settings → Passwords).
- **Passkeys:** the engine sends them to the Windows Hello dialog.
- **Bookmarks:** Nea keeps them in `%APPDATA%\Nea\bookmarks.json`.

## How to test

These steps use a temporary data folder and local pages on `127.0.0.1`. They
do not sign in to an account.

1. Start Nea with `NEA_DATA_DIR` set to a temporary folder, `NEA_BENCH=1`
   and `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=<port>`
   (refer to [building.md](building.md#script-control)).
2. Serve a page on `127.0.0.1` that calls `window.open` to a page on
   `localhost`. That page sends `window.opener.postMessage(...)` and then
   calls `window.close()`.
3. Click the button with `bench click <selector> --gesture`. Read the
   result of the first page through the DevTools port.
4. For cookies, set a persistent cookie and a session cookie on a local
   page. Read them after `bench discard`, after `bench close` (the engine
   stops) and after `bench quit` and a new start.
