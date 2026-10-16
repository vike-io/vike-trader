//! **The news feed: sixteen RSS 2.0 providers, merged newest-first.** The parsing is the desktop's
//! (`vike-app-core`'s former `tools/news.rs`), moved rather than reinvented — the same `<item>`
//! split, the same CDATA / numeric-entity / named-entity clean-up, the same 25 items per provider,
//! `<category>` tags (at most six) and RFC 2822 publication stamps. What differs is WHO runs it (this
//! process, once, for every desktop) and what a failure does.
//!
//! # ⚠ A provider that fails keeps ITS last good items
//!
//! The desktop rebuilt the list from whichever providers answered, so one slow provider made its
//! headlines vanish for five minutes. [`NewsPoller`] holds each provider's last good items in a slot
//! and merges from all slots, so a failed provider contributes its previous items and the reply's
//! `sources_ok` of `sources_total` says how many answered on the latest poll. A body that is a 200
//! with no `<item>` (a captcha page, an Atom feed) is a FAILURE for the same reason: it must not
//! replace a provider's good items with nothing.
//!
//! # ⚠ MarketWatch moved to its https address
//!
//! The desktop fetched `http://feeds.marketwatch.com/marketwatch/topstories/`, the one plain-http
//! URL in the lane. Measured 2026-10-12: that address answers `301` to the https twin, which answers
//! `301` again to `https://feeds.content.dowjones.io/public/rss/mw_topstories`, an RSS 2.0 document
//! with `<item>` elements — so the row names the final https URL ([`FEEDS`]) and no request in this
//! lane is plain http.
//!
//! # ⚠ Two providers answer a datacenter address differently from a desktop
//!
//! Measured from the CI box on 2026-10-12 (the daemon's box): `https://www.fxstreet.com/rss/news` (and its
//! `/rss`, `/rss/analysis`) answers `403` with a Cloudflare "Just a moment..." challenge, and
//! `https://www.cnbc.com/id/100003114/device/rss/rss.html` answers `403 Access Denied`, each with the
//! `ureq` agent AND a browser `User-Agent`. CNBC's own search host serves the same "Top News" feed
//! (`id=100003114`) as RSS, `200` with 30 `<item>`s in 0.26 s, so the CNBC row names that address.
//! FXStreet has no such twin: the row is ActionForex (`https://www.actionforex.com/feed/`, `200`, 20
//! `<item>`s, RFC 2822 stamps; spare: BabyPips `https://www.babypips.com/feed.rss`, `200`, 14 items).
//! A provider that fails is logged ONCE when it starts failing and once when it recovers
//! (`super::health`), so a `403` like these leaves a line in the journal.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use vike_datahub_client::tool_feeds::{MAX_NEWS_ITEMS, NEWS_SUMMARY_CHARS, NewsItem};

use super::health::ProviderHealth;
use super::worker::PollResult;
use super::{FeedHttp, HttpGet};

/// The per-provider request timeout (the desktop's global timeout for the fan-out).
pub const FEED_TIMEOUT: Duration = Duration::from_secs(12);

/// How many `<item>`s of one provider's feed are taken (newest first, as the feed orders them).
pub const ITEMS_PER_FEED: usize = 25;

/// How many `<category>` values of one item are kept.
pub const TAGS_PER_ITEM: usize = 6;

/// One news provider: display name, market bucket (`crypto` / `forex` / `stocks`) and feed URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewsFeedSpec {
    pub name: String,
    pub market: String,
    pub url: String,
}

/// The sixteen providers (the desktop's table, ported from `news/providers.py`). Every URL is https.
pub const FEEDS: [(&str, &str, &str); 16] = [
    ("CoinDesk", "crypto", "https://www.coindesk.com/arc/outboundfeeds/rss/"),
    ("Cointelegraph", "crypto", "https://cointelegraph.com/rss"),
    ("Decrypt", "crypto", "https://decrypt.co/feed"),
    ("CryptoSlate", "crypto", "https://cryptoslate.com/feed/"),
    ("BeInCrypto", "crypto", "https://beincrypto.com/feed/"),
    ("Bitcoin Magazine", "crypto", "https://bitcoinmagazine.com/feed"),
    ("NewsBTC", "crypto", "https://www.newsbtc.com/feed/"),
    ("CoinJournal", "crypto", "https://coinjournal.net/feed/"),
    ("ActionForex", "forex", "https://www.actionforex.com/feed/"),
    ("ForexLive", "forex", "https://www.forexlive.com/feed/news/"),
    ("FXEmpire", "forex", "https://www.fxempire.com/api/v1/en/articles/rss/news"),
    ("Investing.com", "forex", "https://www.investing.com/rss/news.rss"),
    ("Investing.com FX", "forex", "https://www.investing.com/rss/forex.rss"),
    ("MarketWatch", "stocks", "https://feeds.content.dowjones.io/public/rss/mw_topstories"),
    (
        "CNBC",
        "stocks",
        "https://search.cnbc.com/rs/search/combinedcms/view.xml?partnerId=wrss01&id=100003114",
    ),
    ("Seeking Alpha", "stocks", "https://seekingalpha.com/market_currents.xml"),
];

/// [`FEEDS`] as owned specs — the production table.
pub fn production_feeds() -> Vec<NewsFeedSpec> {
    FEEDS
        .iter()
        .map(|&(name, market, url)| NewsFeedSpec {
            name: name.to_string(),
            market: market.to_string(),
            url: url.to_string(),
        })
        .collect()
}

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let i = s.find(a)? + a.len();
    let j = s[i..].find(b)? + i;
    Some(&s[i..j])
}

/// RSS descriptions are HTML: strip CDATA wrappers and tags, decode the common entities, collapse
/// whitespace.
fn clean_html(s: &str) -> String {
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

/// **Parse one provider's RSS 2.0 body** into at most [`ITEMS_PER_FEED`] items: an item with an empty
/// title is skipped, the summary is clipped to [`NEWS_SUMMARY_CHARS`] characters, an unparsable
/// `pubDate` is `ts_ms == 0`.
pub fn parse_rss(body: &str, source: &str, market: &str) -> Vec<NewsItem> {
    let mut out = Vec::new();
    for item in body.split("<item>").skip(1).take(ITEMS_PER_FEED) {
        let title = clean_html(between(item, "<title>", "</title>").unwrap_or(""));
        if title.is_empty() {
            continue;
        }
        let summary = clean_html(between(item, "<description>", "</description>").unwrap_or(""));
        let url =
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
            if !clean.is_empty() && tags.len() < TAGS_PER_ITEM {
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
            summary: summary.chars().take(NEWS_SUMMARY_CHARS).collect(),
            url,
            market: market.to_string(),
            tags,
            ts_ms,
        });
    }
    out
}

/// **Merge the providers' slots** newest-first (stable across providers, in table order) and cut to
/// [`MAX_NEWS_ITEMS`].
pub fn merge_slots(slots: &[Option<Vec<NewsItem>>]) -> Vec<NewsItem> {
    let mut out: Vec<NewsItem> = slots.iter().flatten().flatten().cloned().collect();
    out.sort_by_key(|b| std::cmp::Reverse(b.ts_ms));
    out.truncate(MAX_NEWS_ITEMS);
    out
}

/// The news poll: the provider table, the transport, and one slot of last good items per provider.
pub struct NewsPoller {
    feeds: Vec<NewsFeedSpec>,
    http: Arc<dyn FeedHttp>,
    slots: Mutex<Vec<Option<Vec<NewsItem>>>>,
    health: ProviderHealth,
}

impl NewsPoller {
    pub fn new(feeds: Vec<NewsFeedSpec>, http: Arc<dyn FeedHttp>) -> Self {
        let slots = Mutex::new(vec![None; feeds.len()]);
        Self { feeds, http, slots, health: ProviderHealth::new() }
    }

    fn fetch_one(&self, feed: &NewsFeedSpec) -> Result<Vec<NewsItem>, String> {
        let body = self.http.get(&HttpGet::new(feed.url.as_str(), FEED_TIMEOUT))?;
        let items = parse_rss(&body, &feed.name, &feed.market);
        if items.is_empty() {
            return Err("the body held no <item> with a title".to_string());
        }
        Ok(items)
    }

    /// **One poll of every provider**, in parallel (sixteen sequential fetches would take a minute),
    /// folded into the slots.
    pub fn poll(&self) -> PollResult<NewsItem> {
        let total = u32::try_from(self.feeds.len()).unwrap_or(u32::MAX);
        let answers: Vec<Result<Vec<NewsItem>, String>> = std::thread::scope(|scope| {
            let handles: Vec<_> = self
                .feeds
                .iter()
                .map(|feed| {
                    std::thread::Builder::new()
                        .name("dh-toolfeeds-rss".into())
                        .spawn_scoped(scope, move || self.fetch_one(feed))
                })
                .collect();
            handles
                .into_iter()
                .map(|spawned| match spawned {
                    Ok(handle) => {
                        handle.join().unwrap_or_else(|_| Err("the fetch panicked".to_string()))
                    }
                    Err(e) => Err(format!("could not spawn the fetch thread: {e}")),
                })
                .collect()
        });
        let mut ok = 0u32;
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        for (i, answer) in answers.into_iter().enumerate() {
            match answer {
                Ok(items) => {
                    ok += 1;
                    slots[i] = Some(items);
                    self.health.record("news", &self.feeds[i].name, "", Ok(()));
                }
                // The provider NAME and the reason, once per transition (`health`); never the
                // URL (a debugging convenience is not worth a line that could one day carry a
                // token in a query string).
                Err(why) => self.health.record("news", &self.feeds[i].name, "", Err(why.as_str())),
            }
        }
        if ok == 0 {
            return PollResult::Failed { total };
        }
        PollResult::Rows { rows: merge_slots(&slots), ok, total }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>Feed</title>
<item>
  <title><![CDATA[Bitcoin &amp; Ether rally &#8217;s end &#x2014; what&apos;s next]]></title>
  <link>
     https://example.test/a
  </link>
  <description><![CDATA[<p>Prices <b>rose</b> 5%&nbsp;today &lt;3 &quot;quoted&quot;</p><img src="x.png"/>]]></description>
  <pubDate>Mon, 12 Oct 2026 06:30:00 +0000</pubDate>
  <category>Markets</category>
  <category><![CDATA[Crypto &amp; DeFi]]></category>
  <category></category>
</item>
<item>
  <title></title>
  <link>https://example.test/skipped</link>
</item>
<item>
  <title>No date, no tags</title>
  <link>https://example.test/b</link>
  <pubDate>not a date</pubDate>
</item>
</channel></rss>"#;

    #[test]
    fn rss_with_cdata_and_entities_parses_to_clean_text() {
        let items = parse_rss(RSS, "Prov", "crypto");
        assert_eq!(items.len(), 2, "the empty-title item is skipped");
        let a = &items[0];
        assert_eq!(a.title, "Bitcoin & Ether rally \u{2019}s end \u{2014} what's next");
        assert_eq!(a.summary, "Prices rose 5% today <3 \"quoted\"");
        assert_eq!(a.url, "https://example.test/a", "the link is trimmed");
        assert_eq!(a.ts_ms, 1_791_786_600_000, "RFC 2822 to epoch ms UTC");
        assert_eq!(a.tags, ["Markets", "Crypto & DeFi"], "blank categories are dropped");
        assert_eq!((a.source.as_str(), a.market.as_str()), ("Prov", "crypto"));
        let b = &items[1];
        assert_eq!((b.ts_ms, b.tags.len(), b.summary.as_str()), (0, 0, ""));
    }

    #[test]
    fn a_summary_is_clipped_and_a_feed_to_twenty_five_items_and_tags_to_six() {
        let long = "x".repeat(NEWS_SUMMARY_CHARS + 50);
        let cats: String = (0..9).map(|i| format!("<category>c{i}</category>")).collect();
        let mut body = String::from("<rss>");
        for i in 0..40 {
            body.push_str(&format!(
                "<item><title>t{i}</title><description>{long}</description>{cats}</item>"
            ));
        }
        let items = parse_rss(&body, "P", "m");
        assert_eq!(items.len(), ITEMS_PER_FEED);
        assert_eq!(items[0].summary.chars().count(), NEWS_SUMMARY_CHARS);
        assert_eq!(items[0].tags.len(), TAGS_PER_ITEM);
        assert_eq!(items[24].title, "t24");
    }

    #[test]
    fn a_non_rss_body_parses_to_nothing() {
        assert!(parse_rss("<html><body>Just a moment...</body></html>", "P", "m").is_empty());
        assert!(parse_rss("", "P", "m").is_empty());
        // an Atom feed has <entry>, not <item>: the desktop showed nothing for it, so do we
        assert!(parse_rss("<feed><entry><title>x</title></entry></feed>", "P", "m").is_empty());
    }

    #[test]
    fn a_numeric_reference_that_is_not_one_survives_literally() {
        assert_eq!(clean_html("a &#zz; b &# c &#9999999999; d"), "a &#zz; b &# c &#9999999999; d");
        assert_eq!(clean_html("&#65;&#x42;"), "AB");
    }

    fn item(source: &str, ts_ms: i64) -> NewsItem {
        NewsItem {
            source: source.into(),
            title: format!("{source}-{ts_ms}"),
            summary: String::new(),
            url: String::new(),
            market: "m".into(),
            tags: vec![],
            ts_ms,
        }
    }

    #[test]
    fn the_merge_is_newest_first_stable_across_providers_and_bounded() {
        let slots = vec![
            Some(vec![item("a", 30), item("a", 10)]),
            None,
            Some(vec![item("c", 30), item("c", 20)]),
        ];
        let merged = merge_slots(&slots);
        let order: Vec<&str> = merged.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(order, ["a-30", "c-30", "c-20", "a-10"], "ties keep table order");
        let many = vec![Some((0..MAX_NEWS_ITEMS as i64 + 50).map(|t| item("a", t)).collect())];
        let merged = merge_slots(&many);
        assert_eq!(merged.len(), MAX_NEWS_ITEMS);
        assert_eq!(merged[0].ts_ms, MAX_NEWS_ITEMS as i64 + 49, "the cut drops the OLDEST");
    }

    #[test]
    fn the_datacenter_hostile_addresses_are_gone_and_cnbc_names_its_search_host() {
        for (name, _, url) in FEEDS {
            assert!(!url.contains("fxstreet.com"), "FXStreet 403s from a datacenter: {name}");
            if name == "CNBC" {
                assert!(url.starts_with("https://search.cnbc.com/"), "{url}");
            } else {
                assert!(!url.contains("www.cnbc.com"), "{name}: {url}");
            }
        }
    }

    #[test]
    fn no_production_feed_is_plain_http_and_the_table_is_what_the_desktop_had() {
        assert_eq!(FEEDS.len(), 16);
        assert!(FEEDS.iter().all(|(_, _, url)| url.starts_with("https://")), "{FEEDS:?}");
        let names: Vec<&str> = FEEDS.iter().map(|(n, _, _)| *n).collect();
        assert!(names.contains(&"MarketWatch") && names.contains(&"Seeking Alpha"));
        for market in ["crypto", "forex", "stocks"] {
            assert!(FEEDS.iter().any(|(_, m, _)| *m == market), "{market}");
        }
    }

    /// A transport that answers each URL from a table, counting calls.
    struct Table(Mutex<Vec<(String, Result<String, String>)>>);
    impl FeedHttp for Table {
        fn get(&self, request: &HttpGet) -> Result<String, String> {
            let table = self.0.lock().unwrap();
            table
                .iter()
                .find(|(u, _)| u == request.url())
                .map(|(_, r)| r.clone())
                .unwrap_or_else(|| Err("no such url".into()))
        }
    }

    fn spec(name: &str) -> NewsFeedSpec {
        NewsFeedSpec { name: name.into(), market: "m".into(), url: format!("http://t/{name}") }
    }

    fn rss_of(title: &str, ts: &str) -> String {
        format!("<rss><item><title>{title}</title><pubDate>{ts}</pubDate></item></rss>")
    }

    #[test]
    fn a_failing_provider_is_logged_once_by_name_and_reason_never_its_url() {
        let url = "http://t/Keyed?partnerId=SECRETQ&id=1";
        let http = Arc::new(Table(Mutex::new(vec![(url.into(), Err("HTTP 403".into()))])));
        let poller = NewsPoller::new(
            vec![NewsFeedSpec { name: "Keyed".into(), market: "m".into(), url: url.into() }],
            Arc::clone(&http) as Arc<dyn FeedHttp>,
        );
        let (_, lines) = vike_log::capture::captured(|| {
            for _ in 0..3 {
                assert!(matches!(poller.poll(), PollResult::Failed { total: 1 }));
            }
            http.0.lock().unwrap()[0].1 = Ok(rss_of("back", "Mon, 12 Oct 2026 06:00:00 +0000"));
            assert!(matches!(poller.poll(), PollResult::Rows { ok: 1, .. }));
            assert!(matches!(poller.poll(), PollResult::Rows { ok: 1, .. }));
        });
        assert_eq!(lines.len(), 2, "one failure line, one recovery line: {lines:?}");
        assert_eq!(lines[0].level, tracing::Level::WARN);
        assert_eq!(
            (lines[0].field("provider"), lines[0].field("why")),
            (Some("Keyed"), Some("HTTP 403"))
        );
        assert_eq!(lines[1].level, tracing::Level::INFO);
        for e in &lines {
            let text = format!("{e:?}");
            assert!(!text.contains("SECRETQ") && !text.contains("partnerId"), "{text}");
        }
    }

    #[test]
    fn a_provider_that_fails_keeps_its_last_good_items_and_the_count_says_so() {
        let http = Arc::new(Table(Mutex::new(vec![
            ("http://t/a".into(), Ok(rss_of("from a", "Mon, 12 Oct 2026 06:00:00 +0000"))),
            ("http://t/b".into(), Ok(rss_of("from b", "Mon, 12 Oct 2026 07:00:00 +0000"))),
        ])));
        let poller =
            NewsPoller::new(vec![spec("a"), spec("b")], Arc::clone(&http) as Arc<dyn FeedHttp>);
        let PollResult::Rows { rows, ok, total } = poller.poll() else { panic!("rows") };
        assert_eq!((ok, total, rows.len()), (2, 2, 2));
        assert_eq!(rows[0].title, "from b", "newest first");

        // provider b now fails (and a returns a non-RSS page): b's items stay, a's stay
        *http.0.lock().unwrap() = vec![
            ("http://t/a".into(), Ok("<html>captcha</html>".into())),
            ("http://t/b".into(), Err("HTTP 503".into())),
        ];
        assert!(matches!(poller.poll(), PollResult::Failed { total: 2 }), "nobody answered");

        // only a answers, with a NEW item: b's old item is still merged in
        http.0.lock().unwrap()[0].1 = Ok(rss_of("a again", "Mon, 12 Oct 2026 08:00:00 +0000"));
        let PollResult::Rows { rows, ok, total } = poller.poll() else { panic!("rows") };
        assert_eq!((ok, total), (1, 2));
        let titles: Vec<&str> = rows.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["a again", "from b"]);
    }
}
