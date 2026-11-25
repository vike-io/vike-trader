//! The News providers' bundled icons. The desktop decodes them once with its egui context
//! (`App::load_news_logos`, which keeps the PNG decoder out of this crate).

/// The News providers' icons, by SOURCE NAME exactly as the datahub's `FEEDS` table spells it
/// (`crates/vike-datahub/src/tool_feeds/news.rs`). 64 px PNGs bundled from `assets/news-logos/`
/// (source, date and the licence caveat: its README), so the desktop downloads nothing for them.
/// ⚠ `crates/vike-datahub/tests/default_build/news_logo_assets.rs` reads THIS table as text: one row per
/// feed, written `("<name>", include_bytes!("../assets/news-logos/<slug>.png")),`.
pub const NEWS_LOGO_PNGS: &[(&str, &[u8])] = &[
    ("CoinDesk", include_bytes!("../assets/news-logos/coindesk.png")),
    ("Cointelegraph", include_bytes!("../assets/news-logos/cointelegraph.png")),
    ("Decrypt", include_bytes!("../assets/news-logos/decrypt.png")),
    ("CryptoSlate", include_bytes!("../assets/news-logos/cryptoslate.png")),
    ("BeInCrypto", include_bytes!("../assets/news-logos/beincrypto.png")),
    ("Bitcoin Magazine", include_bytes!("../assets/news-logos/bitcoin-magazine.png")),
    ("NewsBTC", include_bytes!("../assets/news-logos/newsbtc.png")),
    ("CoinJournal", include_bytes!("../assets/news-logos/coinjournal.png")),
    ("ActionForex", include_bytes!("../assets/news-logos/actionforex.png")),
    ("ForexLive", include_bytes!("../assets/news-logos/forexlive.png")),
    ("FXEmpire", include_bytes!("../assets/news-logos/fxempire.png")),
    ("Investing.com", include_bytes!("../assets/news-logos/investing-com.png")),
    ("Investing.com FX", include_bytes!("../assets/news-logos/investing-com-fx.png")),
    ("MarketWatch", include_bytes!("../assets/news-logos/marketwatch.png")),
    ("CNBC", include_bytes!("../assets/news-logos/cnbc.png")),
    ("Seeking Alpha", include_bytes!("../assets/news-logos/seeking-alpha.png")),
];
