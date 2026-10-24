//! The News tool's display layer: the topic classifier and the filter vocabularies. The headlines
//! themselves are the datahub's (`vike_datahub_client::tool_feeds::NewsItem`, asked for by
//! `super::tool_feeds`); nothing here fetches.

/// Coarse topic bucket from a headline (+ feed tags) — a faithful port of `news/classify.py`
/// (first word-boundary keyword rule wins; default "Markets"). Drives the Category filter + chips.
pub fn classify_news(title: &str, tags: &[String]) -> &'static str {
    let mut hay = String::from(" ");
    hay.push_str(&title.to_lowercase());
    hay.push(' ');
    for t in tags {
        hay.push_str(&t.to_lowercase());
        hay.push(' ');
    }
    // (category, keywords) — ordered; trailing-space keys match standalone tokens only.
    const RULES: &[(&str, &[&str])] = &[
        (
            "Earnings",
            &[
                "earnings",
                "quarterly results",
                "earnings results",
                "eps ",
                "guidance",
                "profit",
                "beats estimates",
                "misses estimates",
                "tops estimates",
                "operating margin",
                "net income",
            ],
        ),
        (
            "M&A",
            &[
                "acquire",
                "acquisition",
                "merger",
                "merge",
                "takeover",
                "buyout",
                "to buy",
                "stake in",
                "deal to",
                "in talks to",
            ],
        ),
        (
            "Regulation",
            &[
                "sec ",
                "regulator",
                "lawsuit",
                "antitrust",
                "probe",
                "subpoena",
                "sanction",
                "settlement",
                "indict",
                "banned",
                "to ban",
                "ban on",
            ],
        ),
        (
            "Macro",
            &[
                "inflation",
                "cpi",
                "ppi",
                "gdp",
                "jobless",
                "payroll",
                "unemployment",
                "rate decision",
                "rate cut",
                "rate hike",
                "interest rate",
                "fed ",
                "fomc",
                "ecb ",
                "boe ",
                "central bank",
                "yields",
                "treasury",
                "recession",
                "emergency",
            ],
        ),
        (
            "Commodities",
            &[
                "oil",
                "crude",
                "brent",
                "wti",
                "gold",
                "silver",
                "copper",
                "natural gas",
                "opec",
                "commodit",
            ],
        ),
        (
            "Crypto",
            &[
                "bitcoin",
                "btc",
                "ethereum",
                "eth ",
                "crypto",
                "token",
                "blockchain",
                "defi ",
                "stablecoin",
                "altcoin",
                "solana",
                "xrp",
                "binance",
                "etf inflow",
            ],
        ),
        (
            "Tech",
            &[
                "artificial intelligence",
                "ai ",
                "chip",
                "semiconductor",
                "nvidia",
                "software",
                "cloud",
                "data center",
                "iphone",
                "app store",
                "openai",
            ],
        ),
    ];
    for (cat, kws) in RULES {
        if kws.iter().any(|k| word_boundary_contains(&hay, k)) {
            return cat;
        }
    }
    "Markets"
}

/// `\b`-anchored substring test: `kw` must start at a word boundary (start or after a non-alnum).
fn word_boundary_contains(hay: &str, kw: &str) -> bool {
    let bytes = hay.as_bytes();
    let mut start = 0;
    while let Some(pos) = hay[start..].find(kw) {
        let idx = start + pos;
        if idx == 0 || !bytes[idx - 1].is_ascii_alphanumeric() {
            return true;
        }
        start = idx + 1;
    }
    false
}

pub const NEWS_CATEGORIES: [&str; 8] =
    ["Earnings", "M&A", "Macro", "Crypto", "Regulation", "Commodities", "Tech", "Markets"];
pub const NEWS_MARKETS: [&str; 4] = ["Crypto", "Forex", "Stocks", "Global"];
