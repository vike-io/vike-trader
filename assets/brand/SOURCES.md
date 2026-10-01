# Brand files

The Vike mark and the app icon, as files for the operating systems and packagers that need them
(design system spec §6). Every file here is GENERATED from `crates/vike-ui-theme/src/brand.rs` (the
mark's geometry, the orange and the Graphite tile) by `crates/vike-ui-theme/tests/brand_assets.rs`,
which derives each one again and fails when a committed file differs. Do not edit a file here by
hand: change the geometry, regenerate, and look at the result.

    VIKE_REGEN_BRAND_ASSETS=1 cargo test -p vike-ui-theme --test brand_assets

The SVGs and the desktop entry are compared byte for byte. A PNG, on its own or inside the `.ico`
or the `.icns`, is compared by its decoded pixels: re-compressing a PNG changes nothing, changing a
pixel fails the gate. Every raster at 32 px and below is drawn with the small mark.

| File | What it is | Read by |
|---|---|---|
| `mark.svg` | the vector master in the brand orange | reference artwork: the site, docs, print |
| `mark-small.svg` | the small-size mark: a smaller circle, a lighter diagonal, a wider gap | reference artwork for 32 px and below |
| `mark-on-light.svg` | the master in the darker orange, for light backgrounds | nothing in this repository yet |
| `mark-small-white.svg` | the small mark in white | nothing yet: a Windows tray or macOS menu-bar icon |
| `mark-small-black.svg` | the small mark in black | nothing yet: a Windows tray or macOS menu-bar icon |
| `app-icon.svg` | the app icon: the mark on the Graphite tile | the Linux scalable icon, written by `vike-desktop --install-desktop-entry` |
| `app-icon-small.svg` | the app icon with the small mark | reference for the rasters at 32 px and below |
| `io.vike.Trader.desktop` | the Linux desktop entry, with `Exec=vike-desktop` | nothing reads this copy: `vike-desktop --install-desktop-entry` writes the entry from the same function, with the binary's own path and the project's settings directory in `Exec=` |
| `png/app-icon-16.png`, `png/app-icon-22.png`, `png/app-icon-24.png`, `png/app-icon-32.png`, `png/app-icon-48.png`, `png/app-icon-64.png`, `png/app-icon-128.png`, `png/app-icon-256.png`, `png/app-icon-512.png` | the app icon at each Linux hicolor size | `vike-desktop --install-desktop-entry` |
| `vike-desktop.ico` | the app icon at 16, 20, 24, 32, 40, 48, 64 and 256 px | the icon resource inside `vike-desktop.exe`, embedded by the desktop crate's build script |
| `vike-desktop.icns` | the app icon from 16 to 1024 px, inside Apple's icon grid | nothing yet: a macOS `.app` bundle (no macOS build ships) |
