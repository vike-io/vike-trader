//! The Polymarket [`VenueFeed`] — the rotating-family case, and the reason families exist at all.
//!
//! A Polymarket up/down market lives **5 minutes**. Each window is a different market with a
//! different `condition_id` and different outcome token ids, so a customer cannot subscribe by token
//! id: the ids they picked would be dead before the next flush. They name the FAMILY, and this
//! resolves it to live tokens on every tick.
//!
//! Nothing here is new machinery — the venue already owns all of it, and this is deliberately an
//! assembly rather than a reimplementation:
//!
//! - [`WindowSpec`] / [`RollingWindowPlanner`] compute the current + next window slugs and resolve
//!   them to token ids, with a slug cache, a miss-retry throttle, and a monotonic clock clamp.
//! - [`GammaSlugResolver`] does the by-slug lookup (NOT a browse scan: Gamma's paged browse is
//!   ordered by volume DESC and a just-listed window has ~$0 volume, so a scan would report every
//!   future window as unlisted, forever).
//! - [`Feeds`] is the venue's `DataClient`, sharding token subscriptions across sockets.
//!
//! ## Two things this file decides
//!
//! **The family name IS the slug template minus its `-{unix}` suffix.** `btc-updown-5m` names the
//! windows `btc-updown-5m-{unix}`, which is how Gamma spells that series today. The profile key is
//! therefore a thing a customer can verify by pasting into polymarket.com, not a private alias —
//! and the trailing `-5m` carries the bucket length, so nothing else has to be configured.
//!
//! **`Quotes` is dropped when `Book` is requested** ([`VenueFeed::narrow`]). On this venue
//! `subscribe_book` already emits derived L1 (`PumpMode::Book` → `sink.book()` **and**
//! `sink.quote()`), so subscribing both opens a second socket deriving the same L1 from its own copy
//! of the book and writes every quote row twice.

use std::collections::BTreeSet;
use std::sync::Arc;

use vike_data::live::{DataClient, LiveDataSink};
use vike_polymarket::discovery::{
    FetchSpec, GammaSlugResolver, GammaSource, RollingWindowPlanner, WindowSpec,
};
use vike_polymarket::gamma::GammaClient;
use vike_polymarket::universe::UniverseManager;
use vike_polymarket::Feeds;

use crate::runtime::VenueFeed;
use crate::session::Stream;

/// The venue key, as it appears in the store's `venue=` partition.
pub const VENUE: &str = "polymarket";

/// Split a rolling-family name into its slug template and bucket length.
///
/// `btc-updown-5m` → (`"btc-updown-5m-{unix}"`, 5 minutes). The interval is read off the trailing
/// `-<N>m` token because that is how Gamma labels these series
/// (`vike_polymarket::discovery::interval_label`: `300000 → "5m"`, and an HOURLY series is spelled
/// `60m`, not `1h` — so only the minute form is accepted, matching the venue rather than inventing
/// a friendlier spelling that would resolve to nothing).
pub fn window_spec_for(family: &str) -> Result<WindowSpec, String> {
    let minutes = trailing_interval_minutes(family).ok_or_else(|| {
        format!(
            "polymarket family `{family}` does not end in an interval like `-5m` — a rolling family \
             is named after its window slugs (`btc-updown-5m` ⇒ `btc-updown-5m-<unix>`), and the \
             interval is what says how long a window lasts"
        )
    })?;
    Ok(WindowSpec::every_minutes(minutes, format!("{family}-{{unix}}")))
}

/// The `<N>` of a trailing `-<N>m`, when `N` is a positive integer.
fn trailing_interval_minutes(family: &str) -> Option<i64> {
    let tail = family.rsplit('-').next()?;
    let n: i64 = tail.strip_suffix('m')?.parse().ok()?;
    (n > 0).then_some(n)
}

/// What a subscription resolves to on each tick.
enum Target {
    /// A rolling family: recompute the window slugs from the clock and resolve them.
    Rolling {
        planner: RollingWindowPlanner,
        /// Required by [`RollingWindowPlanner::plan`], and deliberately NEVER committed: only
        /// [`RollingTick::target`](vike_polymarket::discovery::RollingTick::target) — the full
        /// desired set — is used, and `SubscriptionSet` is the single source of truth for what is
        /// actually subscribed. Two stateful views of that could drift apart; one cannot.
        universe: UniverseManager,
        gamma: Box<dyn GammaSource>,
        fetch: FetchSpec,
    },
    /// An explicit token list: the same set forever, and no membership (so it stays per-symbol).
    Fixed(BTreeSet<String>),
}

/// One profile subscription, wired to a live Polymarket [`Feeds`].
pub struct PolymarketFeed {
    family: Option<String>,
    target: Target,
    feeds: Feeds,
}

impl PolymarketFeed {
    /// A rolling family (`btc-updown-5m`) over the real Gamma directory.
    ///
    /// `sink` is the recorder's `RecorderSink`; `Feeds::new` opens NO socket — the first
    /// subscription does — so constructing this is free and cannot fail on the network.
    pub fn family(family: &str, sink: Arc<dyn LiveDataSink>) -> Result<Self, String> {
        Self::family_with_source(family, sink, Box::new(GammaClient), FetchSpec::default())
    }

    /// [`family`](Self::family) with an injected Gamma source — the offline test seam, mirroring
    /// `discovery`'s own fixture-source discipline.
    pub fn family_with_source(
        family: &str,
        sink: Arc<dyn LiveDataSink>,
        gamma: Box<dyn GammaSource>,
        fetch: FetchSpec,
    ) -> Result<Self, String> {
        let spec = window_spec_for(family)?;
        Ok(Self {
            family: Some(family.to_string()),
            target: Target::Rolling {
                planner: RollingWindowPlanner::new(spec),
                universe: UniverseManager::default(),
                gamma,
                fetch,
            },
            feeds: Feeds::new(sink, || {}),
        })
    }

    /// An explicit token-id list. No family ⇒ no group ⇒ per-symbol series.
    pub fn symbols(tokens: &[String], sink: Arc<dyn LiveDataSink>) -> Self {
        Self {
            family: None,
            target: Target::Fixed(tokens.iter().cloned().collect()),
            feeds: Feeds::new(sink, || {}),
        }
    }
}

impl VenueFeed for PolymarketFeed {
    fn venue(&self) -> &str {
        VENUE
    }

    fn family(&self) -> Option<&str> {
        self.family.as_deref()
    }

    fn desired(&mut self, now_ms: i64) -> Result<BTreeSet<String>, String> {
        match &mut self.target {
            Target::Rolling { planner, universe, gamma, fetch } => {
                let resolver = GammaSlugResolver::new(gamma.as_ref(), *fetch);
                // `plan`, not `tick`: planning leaves the UniverseManager uncommitted, and a
                // resolver error aborts the whole pass so the desired set stays UNKNOWN rather than
                // collapsing to empty — which the runtime reads as "change nothing".
                Ok(planner.plan(now_ms, &resolver, universe)?.target)
            }
            Target::Fixed(set) => Ok(set.clone()),
        }
    }

    fn client(&mut self) -> &mut dyn DataClient {
        &mut self.feeds
    }

    /// Drop `Quotes` whenever `Book` is also requested — see the module doc. When `Book` is NOT
    /// requested, `Quotes` is kept: `PumpMode::Quotes` is the cheaper book-bookkeeping-only pump for
    /// a customer who wants L1 without storing full depth.
    fn narrow(&self, requested: &[Stream]) -> Vec<Stream> {
        let has_book = requested.contains(&Stream::Book);
        requested.iter().copied().filter(|s| !(has_book && *s == Stream::Quotes)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_data::NoopSink;
    use vike_polymarket::gamma::GammaMarket;

    #[test]
    fn a_family_names_its_window_slugs() {
        let spec = window_spec_for("btc-updown-5m").unwrap();
        assert_eq!(spec.bucket_ms, 300_000);
        // 1970-01-01T00:07:30Z floors to the 00:05 window ⇒ unix 300.
        assert_eq!(spec.window_slugs(450_000)[0], "btc-updown-5m-300");
    }

    /// The family key must be exactly what the venue calls the series, so a customer can verify it
    /// by pasting the rendered slug into polymarket.com. This pins that `btc-updown-5m` produces
    /// byte-identical slugs to the venue's own `WindowSpec::updown("btc", 300_000)` helper.
    #[test]
    fn the_family_key_agrees_with_the_venues_own_updown_spec() {
        let ours = window_spec_for("btc-updown-5m").unwrap();
        let theirs = WindowSpec::updown("btc", 300_000);
        assert_eq!(ours.slug_template, theirs.slug_template);
        assert_eq!(ours.bucket_ms, theirs.bucket_ms);
        assert_eq!(ours.window_slugs(1_700_000_000_000), theirs.window_slugs(1_700_000_000_000));
    }

    /// An hourly series is spelled `60m` by Gamma, not `1h`. Accepting `1h` here would build slugs
    /// that resolve to nothing and record silence.
    #[test]
    fn only_the_minute_spelling_is_accepted() {
        assert_eq!(trailing_interval_minutes("eth-updown-60m"), Some(60));
        assert_eq!(trailing_interval_minutes("eth-updown-1h"), None);
        assert_eq!(
            trailing_interval_minutes("btc-updown-0m"),
            None,
            "a zero window is not a window"
        );
        assert_eq!(trailing_interval_minutes("some-event-slug"), None);
        assert!(window_spec_for("eth-updown-1h").unwrap_err().contains("`-5m`"));
    }

    struct FixtureGamma(Vec<GammaMarket>);

    impl GammaSource for FixtureGamma {
        fn list(&self, _a: bool, _l: usize, _o: usize) -> Result<Vec<GammaMarket>, String> {
            Ok(self.0.clone())
        }
        fn by_slug(&self, slug: &str, _s: FetchSpec) -> Result<Option<GammaMarket>, String> {
            Ok(self.0.iter().find(|m| m.slug == slug).cloned())
        }
    }

    struct FailingGamma;

    impl GammaSource for FailingGamma {
        fn list(&self, _a: bool, _l: usize, _o: usize) -> Result<Vec<GammaMarket>, String> {
            Err("gamma: connection reset".into())
        }
        fn by_slug(&self, _slug: &str, _s: FetchSpec) -> Result<Option<GammaMarket>, String> {
            Err("gamma: connection reset".into())
        }
    }

    fn market(slug: &str, tokens: &[&str]) -> GammaMarket {
        GammaMarket {
            slug: slug.into(),
            condition_id: format!("0x{slug}"),
            active: true,
            tick_size: 0.01,
            outcomes: vec!["Up".into(), "Down".into()],
            token_ids: tokens.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn feed(gamma: Box<dyn GammaSource>) -> PolymarketFeed {
        PolymarketFeed::family_with_source(
            "btc-updown-5m",
            Arc::new(NoopSink),
            gamma,
            FetchSpec::default(),
        )
        .unwrap()
    }

    /// The current window plus the look-ahead one — a recorder must already be streaming the next
    /// window's book when the current one expires, or every rotation starts with a hole.
    #[test]
    fn the_desired_set_is_the_current_plus_next_windows_tokens() {
        let mut f = feed(Box::new(FixtureGamma(vec![
            market("btc-updown-5m-300", &["A_UP", "A_DOWN"]),
            market("btc-updown-5m-600", &["B_UP", "B_DOWN"]),
        ])));

        let got = f.desired(450_000).unwrap();

        assert_eq!(
            got,
            ["A_DOWN", "A_UP", "B_DOWN", "B_UP"].iter().map(|s| s.to_string()).collect()
        );
    }

    /// A future window Gamma has not listed yet is NORMAL — it is simply absent and retried, not an
    /// error that would freeze the whole subscription.
    #[test]
    fn an_unlisted_future_window_is_not_an_error() {
        let mut f = feed(Box::new(FixtureGamma(vec![market("btc-updown-5m-300", &["A_UP"])])));

        let got = f.desired(450_000).unwrap();

        assert_eq!(got, ["A_UP"].iter().map(|s| s.to_string()).collect());
    }

    /// A Gamma outage must surface as `Err` — the runtime reads that as UNKNOWN and changes nothing.
    /// Returning an empty set here would unsubscribe every live book mid-outage.
    #[test]
    fn a_gamma_outage_is_an_error_not_an_empty_set() {
        let mut f = feed(Box::new(FailingGamma));
        assert!(f.desired(450_000).is_err());
    }

    #[test]
    fn book_supersedes_quotes_but_quotes_alone_survive() {
        let f = feed(Box::new(FixtureGamma(vec![])));
        // `Depth` passes through: polymarket serves none, so `SubscriptionSet` learns `Unsupported`
        // once rather than `narrow` having to know about it. Only the book-implies-quotes overlap is
        // this venue's business.
        assert_eq!(f.narrow(&Stream::ALL), vec![Stream::Trades, Stream::Book, Stream::Depth]);
        assert_eq!(
            f.narrow(&[Stream::Quotes, Stream::Trades]),
            vec![Stream::Quotes, Stream::Trades],
            "without book, the cheaper quote-only pump is exactly what was asked for"
        );
    }

    #[test]
    fn an_explicit_token_list_has_no_family() {
        let mut f = PolymarketFeed::symbols(&["TOK_A".into(), "TOK_B".into()], Arc::new(NoopSink));
        assert_eq!(f.family(), None);
        assert_eq!(f.desired(0).unwrap().len(), 2);
        assert_eq!(f.desired(999_999_999).unwrap().len(), 2, "static: the clock changes nothing");
    }
}
