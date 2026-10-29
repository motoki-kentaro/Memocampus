# Third-party notices

Memocampus itself is licensed under the MIT License (see `LICENSE`).
The material listed below is included from third parties. It is **not** covered by the MIT License;
its own license applies.

This file is kept identical in the repository root and in the `extension/` folder, so that the
extension folder distributed on its own carries the same notices. Paths below are relative to
either location.

## Material Symbols — “settings” icon

| | |
|---|---|
| File | `assets/settings.svg` |
| Material | Material Symbols, icon “settings” (Outlined style) |
| Provider | Google LLC |
| Source | Google Fonts Material Symbols — https://fonts.google.com/icons (documentation: https://developers.google.com/fonts/docs/material_symbols , repository: https://github.com/google/material-design-icons ) |
| License | Apache License, Version 2.0 — full text in `LICENSES/Apache-2.0.txt` (also at https://www.apache.org/licenses/LICENSE-2.0 ) |
| NOTICE | The upstream repository `google/material-design-icons` contains no NOTICE file (checked 2026-10-30), so there is no NOTICE text to carry over. |
| Changes by Memocampus | None. The file is used unmodified and displayed through a CSS mask, which only sets the rendered colour. |

Provenance as checked on 2026-10-30:

- The file was supplied by the project owner as the Google Material Symbols “settings” icon. Google states
  that Material Symbols are available under the Apache License Version 2.0.
- It is **not byte-identical** to the SVG Google currently serves for this icon
  (`https://fonts.gstatic.com/s/i/short-term/release/materialsymbolsoutlined/settings/default/24px.svg`):
  that file uses `viewBox="0 -960 960 960"`, this one `viewBox="0 0 24 24"` with a differently encoded path
  and `fill="#1f1f1f"`.
- Geometrically it matches that official glyph: scaled by 1/40, every one of the 82 official path endpoints
  lies within 0.02 units of an endpoint in this file, and the bounding boxes are identical. This file has two
  additional path points (deviating by at most 0.34 units).
- Which Google download format or tool produced this exact encoding has not been confirmed.
