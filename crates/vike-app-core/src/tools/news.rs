//! The News tool's data: the headline model, the topic classifier and the RSS fetch.

use std::time::Duration;

#[derive(Clone, Default)]
pub struct NewsItem {
    pub source: String, // provider name
    pub title: String,
    pub summary: String,   // plain-text description (HTML stripped)
    pub url: String,       // <link> — drives "Open original"
    pub market: String,    // crypto / forex / stocks / global — drives the Market filter
    pub tags: Vec<String>, // <category> values — drive classify + reader chips
    pub ts_ms: i64,        // pubDate epoch ms (for "Xm ago" + sorting)
}

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

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let i = s.find(a)? + a.len();
    let j = s[i..].find(b)? + i;
    Some(&s[i..j])
}

// --- News (RSS, multiple sources merged) ---
fn clean_html(s: &str) -> String {
    // strip CDATA, tags, and decode the few common entities — RSS descriptions are HTML.
    let s = s.replace("<![CDATA[", "").replace("]]>", "");
    let mut out = String::with_capacity(s.len());
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    // numeric entities first (decimal &#8217; AND hex &#x2019;), then named, then collapse space
    let out = decode_numeric_entities(&out);
    out.replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&") // last, so it can't re-create an entity head
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Decode every `&#NNN;` (decimal) and `&#xHHHH;` (hex) numeric character reference.
fn decode_numeric_entities(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find("&#") {
        result.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        if let Some(semi) = after.find(';') {
            let code = &after[..semi];
            let cp = if let Some(hex) = code.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                code.parse::<u32>().ok()
            };
            if let Some(ch) = cp.and_then(char::from_u32) {
                result.push(ch);
                rest = &after[semi + 1..];
                continue;
            }
        }
        result.push_str("&#"); // not a valid entity — keep the literal and move on
        rest = after;
    }
    result.push_str(rest);
    result
}

fn fetch_one_feed(
    agent: &ureq::Agent,
    source: &str,
    market: &str,
    url: &str,
    out: &mut Vec<NewsItem>,
) {
    let Ok(mut resp) = agent.get(url).call() else { return };
    let Ok(body) = resp.body_mut().read_to_string() else { return };
    for item in body.split("<item>").skip(1).take(25) {
        let title = clean_html(between(item, "<title>", "</title>").unwrap_or(""));
        if title.is_empty() {
            continue;
        }
        let summary = clean_html(between(item, "<description>", "</description>").unwrap_or(""));
        let url_s =
            between(item, "<link>", "</link>").map(|s| s.trim().to_string()).unwrap_or_default();
        let ts_ms = between(item, "<pubDate>", "</pubDate>")
            .and_then(|d| chrono::DateTime::parse_from_rfc2822(d.trim()).ok())
            .map(|d| d.timestamp_millis())
            .unwrap_or(0);
        // all <category> values (RSS topic tags) → chips + classify
        let mut tags: Vec<String> = Vec::new();
        let mut rest = item;
        while let Some(t) = between(rest, "<category>", "</category>") {
            let clean = clean_html(t);
            if !clean.is_empty() && tags.len() < 6 {
                tags.push(clean);
            }
            match rest.find("</category>") {
                Some(p) => rest = &rest[p + "</category>".len()..],
                None => break,
            }
        }
        out.push(NewsItem {
            source: source.to_string(),
            title,
            summary: summary.chars().take(400).collect(),
            url: url_s,
            market: market.to_string(),
            tags,
            ts_ms,
        });
    }
}

pub(super) fn fetch_news() -> Result<Vec<NewsItem>, Box<dyn std::error::Error>> {
    // 16 broad RSS providers across crypto/forex/stocks (ported from news/providers.py)
    const FEEDS: &[(&str, &str, &str)] = &[
        ("CoinDesk", "crypto", "https://www.coindesk.com/arc/outboundfeeds/rss/"),
        ("Cointelegraph", "crypto", "https://cointelegraph.com/rss"),
        ("Decrypt", "crypto", "https://decrypt.co/feed"),
        ("CryptoSlate", "crypto", "https://cryptoslate.com/feed/"),
        ("BeInCrypto", "crypto", "https://beincrypto.com/feed/"),
        ("Bitcoin Magazine", "crypto", "https://bitcoinmagazine.com/feed"),
        ("NewsBTC", "crypto", "https://www.newsbtc.com/feed/"),
        ("CoinJournal", "crypto", "https://coinjournal.net/feed/"),
        ("FXStreet", "forex", "https://www.fxstreet.com/rss/news"),
        ("ForexLive", "forex", "https://www.forexlive.com/feed/news/"),
        ("FXEmpire", "forex", "https://www.fxempire.com/api/v1/en/articles/rss/news"),
        ("Investing.com", "forex", "https://www.investing.com/rss/news.rss"),
        ("Investing.com FX", "forex", "https://www.investing.com/rss/forex.rss"),
        ("MarketWatch", "stocks", "http://feeds.marketwatch.com/marketwatch/topstories/"),
        ("CNBC", "stocks", "https://www.cnbc.com/id/100003114/device/rss/rss.html"),
        ("Seeking Alpha", "stocks", "https://seekingalpha.com/market_currents.xml"),
    ];
    let agent: ureq::Agent =
        ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(12))).build().into();
    // fetch all feeds in parallel (16 sequential RSS hits would be far too slow)
    let handles: Vec<_> = FEEDS
        .iter()
        .map(|&(src, mkt, url)| {
            let agent = agent.clone();
            std::thread::Builder::new()
                .name("tools-feed".into())
                .spawn(move || {
                    let mut v = Vec::new();
                    fetch_one_feed(&agent, src, mkt, url, &mut v);
                    v
                })
                .expect("spawn news-feed thread")
        })
        .collect();
    let mut out = Vec::new();
    for h in handles {
        if let Ok(v) = h.join() {
            out.extend(v);
        }
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.ts_ms));
    out.truncate(500);
    if out.is_empty() {
        return Err("no news".into());
    }
    Ok(out)
}
