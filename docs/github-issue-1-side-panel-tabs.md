# GitHub Issue #1 — side panel tabs [URL] [Image] [⚙]

## Behaviour

- The side panel has a tab list (`role="tablist"`) with **URL**, **Image** and an icon-only settings tab
  (`aria-label="設定"`). Tabs follow the WAI-ARIA tabs pattern: click or ←/→/Home/End (automatic
  activation, roving `tabindex`).
- All three panels stay in the DOM and are only hidden, so the search query, results, pending searches and the
  vault settings state survive switching. Each panel scrolls on its own; the URL tab's scroll offset is saved
  when it is hidden and restored when it is shown again.
- **URL**: the previous search UI unchanged (search, results, click to reopen, incremental loading, record all
  tabs, queue status, errors).
- **Image**: placeholder text only. No image capture, storage, search, tables or context menu.
- **⚙**: the previous vault management UI unchanged (status, create, open, backup, location warnings/approval).
- When the host reports no usable vault at startup, the panel opens on the settings tab. Later, a search that
  fails for lack of a vault shows the error with a “Vault設定を開く” button and a dot on the settings tab,
  without moving the user away from the search box.
- After the vault is switched from the settings tab, URL results are cleared immediately (pending answers from
  the previous vault are discarded) and the search is re-run against the new vault, starting at the top.

Native Messaging messages and the SQLite schema are unchanged.

## Settings icon asset

- Source file: `assets/settings.svg` (provided by the project owner). The extension loads only `extension/`, so
  a byte-identical copy lives at `extension/assets/settings.svg`; `tests/extension/tabs.test.js` fails if the
  two differ. Update both together.
- The icon is drawn with CSS `mask-image` and `currentColor`, so the SVG file and its shape stay unmodified while
  the colour follows the light/dark theme (and system colours in forced-colours mode).
- The SVG is a single `<path>` with no scripts, links or external references.
- Source and license: Google Material Symbols “settings”, Apache License 2.0 (not the project's MIT License).
  The full license text is shipped as `LICENSES/Apache-2.0.txt` and `extension/LICENSES/Apache-2.0.txt`, and
  `THIRD_PARTY_NOTICES.md` (root and `extension/`, identical) records source, license, NOTICE status and
  provenance: it matches the official glyph geometrically after rescaling (within rounding), but is not byte-identical to the
  file Google serves, and the exact download route is unconfirmed.
