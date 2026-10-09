# Assets

## App icon

- **Source:** `yolmira-icon-small.svg`, a 32×32-viewBox SVG: a gold-bordered cobalt tile with a turquoise caravan path and three gold nodes. **Origin:** generated with Google Gemini, then edited by us (the project owner and Claude); added 2026-10-09 with the rename to Yolmira (`docs/decisions.md`).
- **One source for every size.** A detailed raster source (`yolmira-icon-source.png`, for 48 px and up) was planned but not provided, so, as agreed, every size is rendered from the SVG.
- **Generated files** (`app/src-tauri/icons/`): `npx tauri icon ../assets/yolmira-icon-small.svg` (tauri-cli 2.11.5) for the PNG, `.icns` and Windows Square/Store logos. `icon.ico` holds 16, 24, 32, 48, 64, 128 and 256 px, each frame rendered from the SVG at that size (`tauri icon -p <size>`), then packed with Pillow 12.2. Each frame was checked to equal its own render, not a resample of the 256 px one. The Android and iOS sets aren't kept (no mobile target).
- **Check at small sizes:** `yolmira-icon-preview.png` shows every frame at actual size on dark and light backgrounds, with 16 to 48 px also magnified. At 16×16 the tile and the gold three-dot diagonal stay recognisable on both backgrounds. The 1-unit gold border falls on half pixels there and turns soft (greyish on dark, a pale fringe on light); a pixel-aligned 16 px variant would sharpen it if wanted. Corners are transparent at every size, with no halo.
