# Bundled news-source icons

16 PNGs, at most 64 px square, named by the slug of the source name (lowercase, runs of
non-alphanumerics become one `-`: `Investing.com FX` is `investing-com-fx.png`). They back the News
window's source avatars (`crates/vike-desktop/src/app_setup.rs`'s `NEWS_LOGO_PNGS`, keyed by the
source name the datahub's `FEEDS` table spells in `crates/vike-datahub/src/tool_feeds/news.rs`).

- **Source:** `https://www.google.com/s2/favicons?domain=<site>&sz=64`, the same URL the desktop
  used to download at every start (Google's favicon service returns the site's own icon). The sites:
  coindesk.com, cointelegraph.com, decrypt.co, cryptoslate.com, beincrypto.com,
  bitcoinmagazine.com, newsbtc.com, coinjournal.net, actionforex.com, forexlive.com, fxempire.com,
  investing.com (both Investing.com rows), marketwatch.com, cnbc.com, seekingalpha.com.
- **Fetched:** 2026-10-12, once, with curl. Three responses (coindesk, cryptoslate, forexlive) were
  JPEG data and were re-encoded to PNG (same pixels, same size); the rest are committed as served.
  `actionforex.png` was fetched the same day (the Google service answers `301` to a bare request;
  `curl -L`) and replaced `fxstreet.png`: FXStreet answers a datacenter address `403`.
- **Why committed:** the desktop opens no socket for a picture (decision 0143 point 9).
- **Licence:** these are the publishers' own marks. They are used only to identify the source of a
  headline in a list (nominative use). They are NOT covered by this project's licence, and each
  remains its owner's property. They are removed on request: delete the file and its `NEWS_LOGO_PNGS`
  row, and that source falls back to a coloured initial.

A new feed needs a file here and a row in `NEWS_LOGO_PNGS`; `crates/vike-datahub/tests/news_logo_assets.rs`
fails until both exist (or the source is named in its `NO_ICON`).
