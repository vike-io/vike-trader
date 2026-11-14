//! **Every mounted arm's exec-live-or-paper banner, pinned as an operator reads it**: the level, the
//! `tracing` target (the arm's own module), every field and the whole sentence, live and paper (and paper
//! by `data_only` declaration where the arm has one). The six `announce_<venue>_arming` functions each
//! carried their own copy of the WARN line until they shared `warn_arming_banner!`; this holds each one's
//! bytes. The paper remedies of alpaca, oanda and ig (one `feed_gate_disagreement` since the same change)
//! are pinned word for word too.

use std::collections::{BTreeMap, HashSet};

use vike_log::capture::{CapturedEvent, captured};
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::MountPlacement;

use crate::StrategyLanePlanCex;
use crate::venue_arming::arming::{
    ArmingCexAnnouncement, alpaca_arming, cex_arming, ctrader_arming, data_only_arming,
    deribit_arming, ig_arming, oanda_arming,
};

const HEAD: &str = "LIVE assemble_runtime core up; ";

/// The one event an announcement emits, at WARN, from `target`.
fn one_warn(events: Vec<CapturedEvent>, target: &str) -> CapturedEvent {
    assert_eq!(events.len(), 1, "one banner line: {events:#?}");
    let event = events.into_iter().next().expect("one");
    assert_eq!(event.level, tracing::Level::WARN, "{event:#?}");
    assert_eq!(event.target, target, "the arm's own target: {event:#?}");
    event
}

/// The banner's fields, as the capture renders them, for a one-mount start on `config` under `arming`.
fn fields(
    venue: &str,
    config: &MountPlacement,
    arming: &ArmingCexAnnouncement,
    live: &HashSet<String>,
) -> BTreeMap<String, String> {
    [
        ("venue", venue.to_string()),
        ("symbol", config.symbol.clone()),
        ("interval", config.interval.clone()),
        ("tick_size", format!("{:?}", config.tick_size)),
        ("seed_cash", format!("{:?}", config.seed_cash)),
        ("mounts", "1".to_string()),
        ("requote_lanes", arming.requote_lanes.to_string()),
        ("quote_source", arming.quote_source.to_string()),
        ("exec", arming.exec.to_string()),
        ("network", arming.network.to_string()),
        ("live_venues", format!("{live:?}")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

fn live_set(keys: &[&str]) -> HashSet<String> {
    keys.iter().map(|k| (*k).to_string()).collect()
}

/// A credentialed-data arm's two sentences: `feed` is its parenthesis, `found` what the live one says was
/// found, `paper_tail` what follows `EXEC IS NOT LIVE for <venue>` up to the remedy.
struct Words {
    venue: &'static str,
    feed: &'static str,
    found: &'static str,
    paper_tail: &'static str,
}

impl Words {
    fn live(&self, network: &str) -> String {
        format!(
            "{HEAD}{v} feed subscribed ({f}) \u{2014} \u{26a0} EXEC IS LIVE ON {network}: orders from this \
             mount are REAL orders against the {v} {network} account whose {found}",
            v = self.venue,
            f = self.feed,
            found = self.found,
        )
    }

    fn paper(&self, remedy: &str) -> String {
        format!(
            "{HEAD}{v} feed subscribed ({f}) \u{2014} but EXEC IS NOT LIVE for {v} and NO ORDER FROM THIS \
             MOUNT WILL EVER REACH THE VENUE{tail}{remedy}",
            v = self.venue,
            f = self.feed,
            tail = self.paper_tail,
        )
    }
}

type Announce = fn(
    &MountPlacement,
    &[&MountPlacement],
    &HashSet<String>,
    &[vike_config::ArmingAccount],
    &[AccountLabel],
    &HashSet<String>,
);

/// One credentialed arm (alpaca, ctrader, oanda, ig): LIVE, PAPER, and PAPER BY DECLARATION.
fn pin_credentialed(words: &Words, announce: Announce, produce: fn(bool) -> ArmingCexAnnouncement) {
    let venue = words.venue;
    let target = format!("vike_tradehub::feeds::{venue}");
    let config = MountPlacement::new(venue, "SYM", 100_000.0, 0.01);
    let mounts = [AccountLabel::Default];
    let none = HashSet::new();

    let live = live_set(&[venue]);
    let ((), events) = captured(|| announce(&config, &[&config], &live, &[], &mounts, &none));
    let event = one_warn(events, &target);
    let armed = produce(true);
    assert_eq!(event.message, words.live(armed.network), "{venue} live");
    assert_eq!(event.fields, fields(&config.venue, &config, &armed, &live), "{venue} live fields");

    let ((), events) = captured(|| announce(&config, &[&config], &none, &[], &mounts, &none));
    let event = one_warn(events, &target);
    let paper = produce(false);
    let remedy = paper.remedy.clone().expect("paper carries a remedy");
    assert_eq!(event.message, words.paper(&remedy), "{venue} paper");
    assert_eq!(event.fields, fields(&config.venue, &config, &paper, &none), "{venue} paper fields");

    let declared = live_set(&[venue]);
    let ((), events) = captured(|| announce(&config, &[&config], &none, &[], &mounts, &declared));
    let event = one_warn(events, &target);
    let by_declaration = data_only_arming(produce(false), venue);
    let remedy = by_declaration.remedy.clone().expect("a declaration carries its remedy");
    assert!(remedy.contains("DECLARES `data_only = true`"), "{remedy}");
    assert_eq!(event.message, words.paper(&remedy), "{venue} paper by declaration");
}

#[test]
fn alpacas_banner_is_pinned_live_paper_and_declared() {
    let words = Words {
        venue: "alpaca",
        feed: "1m bars + quotes + trades, credentialed WS",
        found: "credentials were found",
        paper_tail: ": ",
    };
    pin_credentialed(&words, super::announce_alpaca_arming, alpaca_arming);
}

#[test]
fn ctraders_banner_is_pinned_live_paper_and_declared() {
    let words = Words {
        venue: "ctrader",
        feed: "spot quotes over the dedicated data socket; closed bars SYNTHESIZED from quote mids",
        found: "tokens were found",
        paper_tail: ": ",
    };
    pin_credentialed(&words, super::announce_ctrader_arming, ctrader_arming);
}

#[test]
fn oandas_banner_is_pinned_live_paper_and_declared() {
    let words = Words {
        venue: "oanda",
        feed: "streamed L1 quotes + POLLED candle closes, both Bearer-authed",
        found: "token was found",
        paper_tail: ": ",
    };
    pin_credentialed(&words, super::announce_oanda_arming, oanda_arming);
}

#[test]
fn igs_banner_is_pinned_live_paper_and_declared() {
    let words = Words {
        venue: "ig",
        feed: "Lightstreamer L1 quotes + streamed candle closes, both over one IG login",
        found: "credentials were found",
        paper_tail: ": ",
    };
    pin_credentialed(&words, super::announce_ig_arming, ig_arming);
}

/// deribit has no `data_only` set (its feed is keyless), says where its prices come from on the live line
/// and "To arm it" on the paper one.
#[test]
fn deribits_banner_is_pinned_live_and_paper() {
    let words = Words {
        venue: "deribit",
        feed: "keyless MAINNET bars + quotes + trades + L2 book",
        found: "credentials were found, priced off the MAINNET book this feed reads",
        paper_tail: ". To arm it: ",
    };
    let target = "vike_tradehub::feeds::deribit";
    let config = MountPlacement::new("deribit", "SYM", 100_000.0, 0.5);
    let mounts = [AccountLabel::Default];

    let live = live_set(&["deribit"]);
    let ((), events) =
        captured(|| super::announce_deribit_arming(&config, &[&config], &live, &[], &mounts));
    let event = one_warn(events, target);
    let armed = deribit_arming(true);
    assert_eq!(event.message, words.live(armed.network));
    assert_eq!(event.fields, fields("deribit", &config, &armed, &live));

    let none = HashSet::new();
    let ((), events) =
        captured(|| super::announce_deribit_arming(&config, &[&config], &none, &[], &mounts));
    let event = one_warn(events, target);
    let paper = deribit_arming(false);
    assert_eq!(event.message, words.paper(&paper.remedy.clone().expect("a remedy")));
    assert_eq!(event.fields, fields("deribit", &config, &paper, &none));
}

/// The CEX arm's banner names the PLAN's slug in its `venue` field and its sentences (never the config's
/// venue), and its paper sentence names the mount's route keys and why the account is paper.
#[test]
fn the_cex_banner_is_pinned_live_and_paper() {
    let venue = StrategyLanePlanCex::ALL[0];
    let slug = venue.slug();
    let target = "vike_tradehub::feeds::cex";
    // A config venue that is NOT the slug: the banner must still say the slug.
    let config = MountPlacement::new("config-venue", "SYM", 100_000.0, 0.1);
    let mounts = [AccountLabel::Default];

    let live = live_set(&[slug]);
    let ((), events) = captured(|| {
        super::announce_cex_arming(
            &venue,
            &false,
            slug,
            &config,
            &[&config, &config],
            &live,
            &[],
            &mounts,
        )
    });
    let event = one_warn(events, target);
    let armed = cex_arming(venue, false, true);
    assert_eq!(
        event.message,
        format!(
            "{HEAD}{slug} feed subscribed (klines + quote/trade/book pump) \u{2014} \u{26a0} EXEC IS LIVE \
             ON {n}: orders from this mount are REAL orders against the {slug} {n} account whose \
             credentials were found",
            n = armed.network
        )
    );
    let mut want = fields(slug, &config, &armed, &live);
    want.insert("mounts".to_string(), "2".to_string());
    assert_eq!(event.fields, want);

    let none = HashSet::new();
    let ((), events) = captured(|| {
        super::announce_cex_arming(&venue, &false, slug, &config, &[&config], &none, &[], &mounts)
    });
    let event = one_warn(events, target);
    let paper = cex_arming(venue, false, false);
    let remedy = paper.remedy.clone().expect("a remedy");
    assert_eq!(
        event.message,
        format!(
            "{HEAD}{slug} feed subscribed (klines + quote/trade/book pump) \u{2014} but EXEC IS NOT LIVE for \
             {slug}: the account this mount trades got no live {slug} client in this process (its row \
             is at tier `paper`, or its exec keys were withheld on purpose by `data_only`), so NO ORDER \
             FROM THIS MOUNT WILL EVER REACH THE VENUE. An account that could not run at its tier would \
             be NOT RUNNING and would have refused this start, not landed here. To change it: {remedy}"
        )
    );
    assert_eq!(event.fields, fields(slug, &config, &paper, &none));
}

/// **The feed-gate-disagreement remedies, word for word** (alpaca, oanda, ig), and none on a live mount.
#[test]
fn the_credentialed_paper_remedies_are_pinned() {
    let said = |upper: &str, fields: &str| {
        format!(
            "the feed gate resolved the DEFAULT {upper} account's {fields} (this mount could not have \
             started otherwise) yet `assemble_runtime` recorded no live {upper} exec \u{2014} the two \
             gates read ONE account, so this state is a daemon bug to report, not a missing key; \
             orders go to the paper book until it is resolved"
        )
    };
    assert_eq!(
        alpaca_arming(false).remedy.as_deref(),
        Some(said("ALPACA", "CLIENT_ID/CLIENT_SECRET/ACCOUNT_ID trio").as_str())
    );
    assert_eq!(
        oanda_arming(false).remedy.as_deref(),
        Some(said("OANDA", "API_KEY/ACCOUNT_ID pair").as_str())
    );
    assert_eq!(
        ig_arming(false).remedy.as_deref(),
        Some(said("IG", "API_KEY/IDENTIFIER/PASSWORD trio").as_str())
    );
    for live in [alpaca_arming(true), oanda_arming(true), ig_arming(true)] {
        assert_eq!(live.remedy, None, "{live:?}");
    }
}
