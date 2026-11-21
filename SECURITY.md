# Security

## Report a vulnerability

**Do not open a public issue.** Report a security problem privately. Then
the users of Nea get the fix before other people know about the problem.

In the GitHub repository, go to the *Security* tab → *Report a vulnerability*
([direct link](https://github.com/Agustin-Delgado/nea/security/advisories/new)).
This makes a private security advisory. It is the only channel. Only the
maintainer of the project can see it.

If possible, include these items:

- The version of Nea (Settings → Updates) and of Windows.
- What happens and what you expected.
- The steps to reproduce the problem, or a proof of concept.
- The impact: what an attacker can do with the problem.

## Next steps

1. We confirm that we received the report within **3 business days**.
2. We assess the report. Within **10 days**, we tell you the severity and the
   plan.
3. We publish the fix through the signed updater. Then we publish an advisory
   that credits you. If you prefer, the advisory does not name you.

Do not disclose the problem until the fix is published or for 90 days after
the report. The shorter period applies.

## Scope

These areas are of special interest:

- The updater: the Ed25519 signature check, the replacement of the
  executable and the downloads (`src/updater.rs`, `src/http.rs`).
- The per-user installer (`src/installer.rs`).
- The script injection into pages (reader mode, hide element, floating
  video).
- Any way for a page to read or change the local data of Nea.

Report problems in the WebView2 engine itself to Microsoft
(<https://msrc.microsoft.com>). If they affect Nea in a specific way, also
tell us.

## Supported versions

Only the latest published version gets fixes. The updater installs it
automatically once a day.
