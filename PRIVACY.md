# Privacy policy

cryo is a browser: it fetches what you navigate to, because that is the
request. It has no telemetry, no analytics, and no crash reporting —
there is no server on the author's side that receives anything. This is
the complete list of what leaves the machine and what stays.

## What leaves the machine

- **Your browsing.** Pages you open, including any forms you submit and
  accounts you sign into, talk to those sites directly.
- **The update check** *(GitHub-installed builds only)*. Release builds
  fetch `latest.json` from this repository's Releases at launch to
  notice a newer version — a plain HTTPS GET carrying no identifier
  beyond what the request implies. Store-installed builds update through
  the Microsoft Store and never make this request.
- **The account button.** If your profile already holds Google cookies,
  cryo sends them to `accounts.google.com/ListAccounts` to label the
  button with the account you are signed into. Incognito windows never
  ask.
- **Extension installs.** Installing an extension fetches it from the
  Chrome Web Store, when you ask for it.
- **Microsoft Store install/updates.** If you install cryo from the
  Store, Microsoft's own
  [privacy statement](https://privacy.microsoft.com/) covers that
  channel.

## What stays on the machine

- **History, bookmarks, saved passwords, and extensions** live under
  your user profile (`%LOCALAPPDATA%\cryo`, or the package's private
  storage for a Store install). Passwords are sealed with DPAPI against
  your Windows account. Nothing in the profile is sent anywhere — the
  only reads are the browser's own.
- **Omnibox suggestions** are computed locally from your history,
  bookmarks and dial list. Keystrokes never leave the machine; there is
  no remote suggest service to send them to.
- **Find-in-page queries** live in the find field for as long as the bar
  is open and are never written to disk.
- **Incognito windows** keep nothing at all: no history, no kept pages,
  no passwords, no window state.

## Questions

Open an issue at <https://github.com/konstantinos193/cryo/issues>.
