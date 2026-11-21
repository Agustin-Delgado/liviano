# Privacy and data

## Blocking and privacy

All blocking controls are in two locations. The tab menu applies to one
site. Settings → Privacy applies to all sites.

### Ads and trackers (network)

Nea uses EasyList and EasyPrivacy and downloads them once a day. "Block ads
on this site" in the tab menu turns off the blocking for that site. It also
turns off the pop-up and cookie banner blocking for that site. It does not
change the tracking prevention of the engine.

### Pop-ups

- Nea always blocks a pop-up that a page opens without a click or a key
  press.
- Nea blocks a pop-up that opens after a click only if the lists identify its
  address as an ad. The lists use `$popup` rules or block a full ad server.
  This stops the popunders that open on the first click.
- Sign-in pop-ups open (Google, Microsoft, Facebook, Apple, GitHub, PayPal). A
  test with the real lists checks this.
- When Nea blocks a pop-up, the page shows "Pop-up blocked". To allow pop-ups
  for the site, select "Allow pop-ups on this site" in the tab menu.
- In a private tab, this permission stays until Nea closes. Nea does not save
  it.
- Nea does not change extension pages.

### Trackers and third-party cookies

The setting is "Block third-party cookies and trackers" in Settings →
Privacy. It is on by default. It uses the *strict* level of the engine's
tracking prevention.

- Measurements show that this level stops scripts from Facebook, Google
  Analytics and Hotjar on other sites. The balanced level, the engine default,
  did not stop them.
- Sign-ins from Google, Microsoft, GitHub and Amazon continue to work.
- The engine blocks the cookies and the storage of these trackers. It does
  not block the cookies of the site that you visit, such as session cookies.
- The engine has no site exceptions for this level. The ad blocking exception
  of a site does not change it.
- Nea does not block third-party cookies from unknown trackers. WebView2 has
  no supported option for this. The only option that works is
  `--test-third-party-cookie-phaseout`. It is a Chromium test switch with no
  WebView2 documentation, so Nea does not use it.

### Cookie banners

The same setting controls cookie banners. Nea uses the EasyList Cookie List
from `secure.fanboy.co.nz`, which it downloads with the other lists.

- The list hides consent notices with its `##`, `domain##` and `#@#` rules.
- For each site, the list stops the consent-manager scripts that it names.
- When a document starts, it asks Nea for the rules of its site (about 180).
  Then it asks for the generic rules of each `#id` and `.class` that it
  contains, as uBlock does.
- Until the first frame, the document asks without a wait. Thus a banner does
  not appear before Nea hides it. These requests stay on the local computer.
- If Nea loaded all 15,000 generic rules into each page, each tab would use
  ~10 MB more. With this method, a tab uses less than 1 MB more. The page
  becomes ready ~15 ms later.
- If a banner stopped the page from scrolling, Nea lets the page scroll again.
- The list does not hide "accept or pay" walls (Spiegel, Corriere, The
  Guardian). This is intentional.
- The compiled list uses ~1.3 MB of Nea memory. When the setting is off, Nea
  does not compile it.

## Local data

Nea has no accounts, no sync, no telemetry and no crash reports. Nea sends
only these items to the network: the pages that you open, their icons and the
daily update check. You can turn off the update check. The field sends
nothing until you press Enter. The suggestions come from the local history.

| Data | Location |
|---|---|
| Settings, open tabs, history, bookmarks, downloads, hidden elements, extensions | `%APPDATA%\Nea\*.json` |
| Filter lists (ads, tracking, cookies) and Web Store extensions | `%APPDATA%\Nea\shield`, `%APPDATA%\Nea\extensions` |
| Sites with a blocking exception or with pop-ups allowed | `settings.json` (`shield_disabled`, `popups_allowed`) |
| Passwords | Windows Credential Manager (generic credentials `Nea:<origin> <username>`) |
| Site icons, up to 4 MB (never from private tabs) | `%APPDATA%\Nea\favicons` |
| Cookies, cache and site data | `%LOCALAPPDATA%\Nea\engine` (a separate profile, not the Edge profile) |
| Downloaded update | `%LOCALAPPDATA%\Nea\update` |
| Installed program | `%LOCALAPPDATA%\Programs\Nea` |

Nea writes the JSON files atomically: it writes a temporary file and then
replaces the old file. Thus a power failure does not leave a partial file.
The uninstaller removes the program and the downloaded update. It keeps the
data for a new installation. To remove the data, delete these folders.
