# Bundled country flags

26 PNGs, 40 px wide, named by lowercase ISO 3166-1 alpha-2 code (`eu` is the European Union's
flag). They back the Calendar tool's currency column (`crates/vike-desktop/src/app_setup.rs`'s `FLAG_PNGS`).

- **Source:** `https://flagcdn.com/w40/<code>.png` (Flagpedia), the same URL the desktop used to
  download at every start.
- **Fetched:** 2026-10-12, once, with curl, and committed unchanged.
- **Why committed:** the desktop opens no socket for a picture (decision 0128's thin-client rule,
  extended to the tool feeds).
- **Licence:** these are renderings of national flags, state symbols that Flagpedia serves free to
  use without attribution. No licence file ships with the images; if one is wanted, ask the source.

To change the set, re-fetch the same URL pattern, add the file here and a row to `FLAG_PNGS`.
