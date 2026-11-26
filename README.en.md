# Memocampus

[日本語](README.md) | **English**

Memocampus records the web pages, images and page originals you view in Google Chrome on Windows into a "Vault" on your own PC, so that you can find them again later.

> [!IMPORTANT]
> Memocampus is currently under review in the Chrome Web Store and has not been released to the public yet. The release is planned as v0.8.0; how to get it will be added to this README when it is published.

## 1. What Memocampus is

- **Automatic recording**: the URL, title and time of the pages you open in Chrome are recorded in the Vault. Recording does not begin until you press "Start remembering" during the initial setup.
- **Manual saving**: from the context menu you can save images, or a whole page (in MHTML format), to the Vault.
- **Search and review**: the side panel and the History page let you find records by URL, title and date.
- **Stored locally**: records are stored in the Vault (an SQLite file) on your PC. A new Vault is encrypted by default.
- **AI integration (optional)**: only if you approve it, an MCP client (an AI app such as Claude Desktop) can read part of the Vault. Without your approval, no records are given to MCP clients.

> [!NOTE]
> Normal records are stored in the local Vault. However, once you approve MCP integration, data given to an MCP client is under that client's control, and depending on the client it may be sent to its AI provider.

Privacy policy: <https://memocampus.github.io/en/privacy/>

## 2. Requirements

- Windows 10 / 11 (x64). Actual operation has been checked only in a Windows 11 VM, not on Windows 10 (section 3).
- Google Chrome 116 or later (Windows)
- Both of these two parts are needed:
  - **The "Memocampus" Chrome extension**: the screens, and accepting what is recorded.
  - **The "Memocampus" Windows app**: saving into the Vault (Native Host), the MCP server and the tray program. It is installed with the installer `Memocampus-Setup-x64.exe`.

The extension cannot record on its own. Without the Windows app, the extension shows "Cannot connect to the Native Host. Check that it is installed and registered."

The extension and the tray are available in Japanese and English. The extension follows Chrome's display language and the tray follows Windows' display language; any other language gets English.

## 3. Getting and installing (currently under review)

### Current status

- The Chrome extension is under review in the Chrome Web Store. Even after it passes review, it cannot be installed until it is published.
- The installer of the Windows app will be provided with the release. Where to get it will be added to this README at that time.
- How to build and try it from source is in the [developer guide](docs/development.md) (for developers).

### What the installer does

The installer installs for the signed-in Windows user only. No administrator rights are needed.

| What | Details |
|---|---|
| Programs | Places three programs (Native Host, MCP server, tray) in `%LOCALAPPDATA%\Programs\Memocampus` |
| Connection with Chrome | Registers the Native Host for this user in the registry (HKCU) so that Chrome can start it. The only extension allowed to connect is the Chrome Web Store version of Memocampus |
| Start menu | Adds "Memocampus" (the tray) |

What the installer does **not** do: install the extension, create a Vault, start recording, approve MCP integration, or change `%LOCALAPPDATA%\Memocampus` (where the Vault and settings are kept). You do all of these yourself after installing.

The last page has two choices, both off by default. When the tray is started for the first time, starting the tray at Windows logon and starting the MCP server automatically are both turned on. You can change both in the tray's menu ("Start with Windows", "Start MCP server automatically"). Even while the MCP server runs, it returns nothing from the Vault to MCP clients until you approve MCP access for that Vault.

> [!WARNING]
> The current installer is not code-signed, so Windows SmartScreen may show a warning (unknown publisher). Run it only if where you got it and the file's SHA-256 match what is officially published.

### What has been confirmed

- Confirmed: the project owner checked the following in an isolated Windows 11 VM:
  - installing with the product installer built by GitHub Actions
  - the Native Messaging Host being registered
  - connecting from a test extension that reproduces the official extension ID
  - saving to the Vault
  - starting the MCP server
- Not confirmed yet: an actual install from the Chrome Web Store, updating, uninstalling, operation on Windows 10, and the other acceptance items (`installer/E2E-ACCEPTANCE.md`).

## 4. Initial setup

1. Press the Memocampus button in Chrome's toolbar to open the side panel.
2. Prepare a Vault in the "Settings" tab (⚙).
   - Choose "Encrypt (recommended)" or "Do not encrypt" (see [section 7](#7-vault-encryption-backup-and-restore) for the difference).
   - Choose "Create in the default location" (`%LOCALAPPDATA%\Memocampus\memocampus.sqlite3`), "Choose a location and create…", or "Open existing Vault…".
3. For an encrypted Vault, the **Recovery Spell** is shown once. It has 43 characters (`A–Z a–z 0–9 - _`, case-sensitive). Write it down, press "I have written it down. Continue to the check", and enter all 43 characters again to finish creating the Vault.
4. Press "Open the first-run guide" in the "URL" tab. The Privacy settings page opens, where you can review and edit the initial settings, such as the rules for what is never recorded, as a draft.
5. When you have reviewed them, press "Start remembering". Automatic recording starts only now. Pages you viewed before this are not recorded afterwards.
6. Only if you want MCP integration, approve it separately with "Review and approve MCP access…" ([section 8](#8-mcp-integration)).

> [!WARNING]
> The Recovery Spell is shown only once, when the Vault is created. It cannot be shown again.
> - With the Vault file and the Recovery Spell, the Vault can be opened on another PC or after reinstalling Windows. If you lose the spell, you cannot recover it on another PC or restore a backup of the encrypted Vault.
> - Creating a backup of an encrypted Vault also needs the spell. If you lose it, you cannot create backups until you reissue it (section 7).
> - Anyone who knows the spell and obtains the Vault file can read its contents. Keep it in a safe place, separate from this PC, where others cannot see it.
> - Entering it again only checks what you typed; it does not prove that the spell is stored safely.

## 5. Basic use

The side panel has four tabs: "URL", "Image", "Page" and "Settings" (⚙).

### URL: searching your records

- Search the recorded visits by part of a URL or title. The calendar button ("Filter by visit date") narrows the results to a date.
- "Record all tabs" records the URL and title of every open normal tab at once.
- The "History" button ("Open History in a new tab") opens the full-page History.

### History: looking back by date

- The "Calendar" shows the days that have records. Choose a day to list its records.
- "Filter by domain" narrows the list to a domain (match type "Exact" or "Regular expression").

### Image: saving images

- Right-click an image on a web page and choose "Save to Memocampus" to save the original image file to the Vault.
  - Formats: PNG, JPEG, GIF, WebP, AVIF and BMP, up to 32 MiB per image. SVG is not saved.
  - The same image (a file with identical contents) is stored once; saving it again adds a record of the time and URLs (its save history).
- The "Image" tab lists the saved images.
  - Click a tile to show its "Save history" and "Tags".
  - Right-click a tile for "Copy image" and "Export as a file".
  - "Filter by tag" narrows the list to one tag.

### Page: saving pages (MHTML)

- Right-click a page and choose "Save page to Memocampus" to save the page of the tab you clicked in to the Vault as Chrome's MHTML original. A link's target page is not saved.
  - Only `http` / `https` pages can be saved, up to 128 MiB each.
  - Pages in incognito windows are not saved.
- The "Page" tab lists the saved pages.
  - Saves of the same URL are shown together. You can search by part of the URL or title.
  - "Export MHTML" on each save record writes the original, unchanged, to a file.
  - Full-text search of the page contents and viewing MHTML are not supported.
- Saved pages are not offered to MCP clients.

> [!CAUTION]
> An MHTML file contains the page exactly as it was displayed when saved. If you save a page you were signed in to, personal information such as names and messages is included. Choose carefully which pages you save.

### Deleting, and actions on one item

- Each URL, image and page row has delete buttons, such as "Delete this visit", "Delete everything for this URL", "Delete this image" and "Delete this save record".
- A delete button first shows "Confirm deletion" with what will be deleted and how many. Nothing is deleted until you press "Delete".
- "Choose an action…" adds one of these rules for that URL (section 6):
  - "Add to the Never Save group"
  - "Add to the Hide from MCP group"
  - "Allow MCP access"

> [!WARNING]
> Deletion cannot be undone. There is no trash.
> - Deletion is done in the application. It does not guarantee that the data can no longer be recovered physically from the disk (the Vault file, SQLite's temporary files, the storage media). The Vault file does not get smaller either.
> - Backups made before the deletion still contain the deleted data.

## 6. Privacy Gate and incognito windows

### Privacy settings (Privacy Gate)

Automatic recording (visiting pages and "Record all tabs") is checked against the rules in the "Privacy settings" every time, before anything is recorded. Open the settings from the first-run guide, or with "Privacy settings" in the Settings tab.

| Group | Effect | What it does not do |
|---|---|---|
| "Never Save group" | Matching pages are no longer recorded automatically from now on | **It does not delete records already in the Vault.** To remove them, delete them as described in section 5 |
| "Hide from MCP group" | Recorded in the Vault, but never shown to MCP clients, in results or in counts | **It does not stop the recording itself** |
| Basic protections | Protect sign-in and authentication pages, URLs that carry credentials and the like from the start | — |

- The rules are shared by all Vaults (`%LOCALAPPDATA%\Memocampus\policy.json`).
- When the settings cannot be read (not initialized, damaged, in use and so on), automatic recording stops, to stay on the safe side. Pages viewed meanwhile are not recorded later. "Check again" in the side panel reads the state again.
- Saving an image or a page by hand happens only when you explicitly do it, and is not subject to the Privacy settings' checks for automatic recording.

### Incognito windows

- Browsing in incognito windows is never recorded automatically, even if you allow the extension in incognito in `chrome://extensions`.
- Pages in incognito windows are not saved by "Save page to Memocampus" either.
- Images in incognito windows can be saved with "Save to Memocampus". Such an image stays in the normal Vault and is included in backups. Its save history records that it was saved from an incognito window. Saves from incognito windows are never published to MCP clients.

## 7. Vault, encryption, backup and restore

### Encrypted and unencrypted Vaults

| | Encrypted Vault (recommended) | Unencrypted Vault ("Do not encrypt") |
|---|---|---|
| Data on disk | Encrypted (SQLCipher) | Not encrypted; anyone who can read the file can read its contents |
| Opening it day to day | The key is protected with Windows DPAPI and stored for this Windows user, so it opens without the spell | Opens as it is |
| On another PC or after a reinstall | Open it with the Vault file and the Recovery Spell, through "Recover with the Recovery Spell…" | Copy the file and open it |
| Recovery Spell | Yes (43 characters) | None |

- An unencrypted Vault can be converted with "Encrypt this Vault", and an encrypted one with "Remove encryption from this Vault". Both create a **new** Vault; the original is neither changed nor deleted.
- If you lose the spell while the Vault still opens, "Reissue the Recovery Spell" creates a new Vault with a new spell. The old spell works only for the old Vault and its backups (see "Backup and restore" below).
- Only Vaults created by this version (schema 10) are supported. A Vault created by an older version is refused and left unchanged; it cannot be converted.

### Backup and restore

- "Create backup…" writes the whole Vault (visits, images and pages included) to one file. There are four formats, depending on the kind of Vault and on password protection:

| Format | Source Vault | Password protection | Needed to create it | Needed to restore it |
|---|---|---|---|---|
| A | Unencrypted Vault | No | — | — (the backup is not encrypted) |
| B | Encrypted Vault | No | Recovery Spell | Recovery Spell |
| C | Unencrypted Vault | Yes | Password | Password |
| D | Encrypted Vault | Yes | Password, Recovery Spell | Password, Recovery Spell |

- For an encrypted Vault (formats B and D), creating a backup checks that the spell you enter can recover that Vault. With a spell that cannot recover it, no backup is created.
- The password is stored nowhere, so if you forget it, the backup cannot be restored.
- A password-protected backup of an unencrypted Vault (format C) cannot be created while the Vault file is in SQLite's WAL mode. Memocampus itself never puts a Vault in WAL mode (it can happen, for example, if another tool changed the Vault file).
- If you lose the spell while the Vault still opens, you can use "Reissue the Recovery Spell". Reissuing creates a **new** Vault with a new spell, whose backups you can then create. It does not replace the old spell: restoring the format B and D backups made before still needs the old spell.
- "Restore from backup…" always restores into a **new** Vault. Existing files and the Vault in use are never overwritten. The Vault in use does not change until you press "Start using this Vault (MCP access not approved)".
- MCP access to a restored, converted or newly created Vault starts as not approved.

> [!WARNING]
> A backup keeps all the data as it was when the backup was made. Data you delete from the Vault later remains in earlier backups. Be careful where you keep backup files and what you do with backups you no longer need. A normal backup of an unencrypted Vault (format A) is not encrypted.

Details: [encrypted Vaults](docs/secure-vault/slice-e.md), [backup and restore](docs/secure-vault/slice-c.md), [conversion](docs/secure-vault/slice-d.md)

## 8. MCP integration

AI clients that support MCP (Model Context Protocol) can search the records in the Vault, read-only.

### Publishing needs your approval

- A Vault is published to MCP only after you press "Review and approve MCP access…" in the Settings tab, review what it says, and press "I have reviewed it: approve MCP access".
- "Stop MCP access" stops publishing at any time.
- When you create, restore, convert or switch Vaults, nothing is published until you approve again.

### What MCP clients get

| Offered | Not offered |
|---|---|
| Searching visits (URL, title, page information), one day's visits, visit counts per domain | Anything in the "Hide from MCP group" or the "Never Save group", and anything that cannot be judged |
| Information about images allowed for publishing, and their **thumbnail images** (never the original image file) | Original image files, image tags |
| — | Saved pages (MHTML) |

An image is offered only when "Allow publishing to MCP" is on for one of its save records in the image's details (it is off right after saving), and that save's image URL, page URL and frame URL are all visible to MCP. Saves from incognito windows are never offered.

### The tray and the MCP server

- Starting "Memocampus" from the Start menu starts the tray, which starts and manages the MCP server. Its menu offers "Start server", "Stop server", "Open configuration file", "Open log folder", "Quit" and more.
- The MCP server that the tray starts uses Streamable HTTP and listens only inside this PC. The default address is `http://127.0.0.1:8765/mcp`; the port can be changed in the tray's configuration file ("Open configuration file"). Every connection needs the token stored in `%LOCALAPPDATA%\Memocampus\mcp-http-token`. Web pages in a browser cannot connect.
- If `memocampus-mcp.exe` is started directly, without the tray, it uses stdio by default. With stdio it opens no HTTP port and talks only to the MCP client that started it, over standard input and output.
- This README does not yet cover how to set up each MCP client. For the technical details, see [Streamable HTTP](docs/mcp-streamable-http.md) and [the MCP server](docs/issue-008-mcp-server.md).

> [!CAUTION]
> Data given to an MCP client leaves Memocampus's control. Depending on the client, it is sent to its AI provider. Check the Vault and the rules before you approve. Also, any program running on this PC as the same Windows user can read the token file.

## 9. Updating and uninstalling

> [!NOTE]
> This section describes the current design. Actual updating and uninstalling have not been confirmed yet (section 3).

### Updating

- Extension: once it is published in the Chrome Web Store, Chrome updates it automatically.
- Windows app: it does not update itself. Running a newer installer updates it in place. A running tray is closed before the update and is not restarted automatically afterwards. The Vault, the settings and the rules are not changed.
- If Chrome or an AI client is using a program of the Windows app (the Native Host or the MCP server), the installer stops without changing anything and asks you to close them and run it again. The installer never forces them to quit.
- If only the extension has been updated, some functions may be unavailable or it may say that the Native Host is outdated. Update the Windows app.

### Uninstalling

- Windows app: uninstall "Memocampus" from "Apps" in Windows Settings. This removes the programs, the Start menu entry, the registration of the connection with Chrome and the tray's logon start entry.
- Extension: remove it in `chrome://extensions`. Uninstalling the Windows app does not remove it. Removing the extension loses the records that have not reached the Vault yet (those still waiting to be sent inside the extension).
- **The Vault, backups, rules, settings and logs (`%LOCALAPPDATA%\Memocampus`) are kept whichever you uninstall.** Delete them yourself if you no longer need them. The same applies to a Vault you created in a location you chose, and to the places where you saved backups.

## 10. Known limitations

- Only Google Chrome 116 or later on Windows 10 / 11 (x64) is supported. It does not work with other browsers or operating systems. It has not been checked on Windows 10 yet.
- It is not published in the Chrome Web Store yet (section 3).
- The installer is not code-signed.
- Vaults created by older versions (schema 9 or earlier) cannot be used or converted.
- Images: images whose site forbids linking from other sites, images that need a login, and `blob:` URLs of a closed page may fail to save. Images above 50 megapixels cannot be copied or shown as thumbnails in the panel (exporting works).
- Pages: full-text search and viewing MHTML are not supported. Some pages cannot be captured by Chrome, and saving them fails.
- Tags can filter by one tag at a time and are not offered to MCP clients.
- Deleting does not make the Vault file smaller, and physical erasure is not guaranteed (section 5).
- The screens are available in Japanese and English only.

## 11. Troubleshooting

| Message or symptom | What to check |
|---|---|
| "Cannot connect to the Native Host. Check that it is installed and registered." | Check that the Windows app is installed. Running the installer again also recreates the registration of the connection with Chrome |
| "No Vault is set up" | In the Settings tab, create a new Vault or open an existing one |
| Nothing is recorded / the URL tab says the settings cannot be read | Check that you pressed "Start remembering". Recording stops while the Privacy settings cannot be read; "Check again" reads them again |
| "…because the Native Host is outdated…" | Update the Windows app to a newer version |
| An encrypted Vault does not open on another PC | Use "Recover with the Recovery Spell…" and enter the spell you wrote down when you created it |
| MCP clients get nothing | Check that MCP access is approved, and the tray's status lines (such as "Vault access"). "Open log folder" in the tray shows the log (`mcp-startup.log`) |
| SmartScreen shows a warning | Read the warning in section 3 |

## 12. For developers, and licence

- Building from source, tests, CI and the repository layout: [developer guide](docs/development.md)
- The Windows installer and the release pipeline: [installer/README.md](installer/README.md)
- Design and acceptance records: [docs/](docs/)

### Licence

The Memocampus source code is released under the MIT License ([LICENSE](LICENSE)). Third-party material is not covered by it:

- `assets/settings.svg`, `assets/cached.svg` and `assets/delete.svg` (and their copies in `extension/assets/`) are Google Material Symbols icons, licensed under the Apache License 2.0. The full text is in [LICENSES/Apache-2.0.txt](LICENSES/Apache-2.0.txt) and the source is recorded in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
- The `extension/` folder carries identical copies of `LICENSE`, `LICENSES/Apache-2.0.txt` and `THIRD_PARTY_NOTICES.md`, so it can be distributed on its own.
- The notices for third-party software contained in the Native Host and MCP server programs are in [NATIVE_THIRD_PARTY_NOTICES.md](NATIVE_THIRD_PARTY_NOTICES.md).
- The Memocampus icon was generated using OpenAI's image generation service. The icon assets are distributed under the project's MIT License.
