use std::sync::{Mutex, PoisonError};

use vike_data::{LiveDataError, SubscriptionId};
use vike_hub_proto::auth::Scope;
use vike_marketdata::{Bar, L2Book, QuoteTick, TradeTick};

use super::*;

/// `build_client`'s own source text, from its signature to its closing brace — what the structural
/// pins below read, because a constructor that is built and dropped shows nothing of how it was
/// configured (the feeds keep their switches in private fields, and the only way to see them work is
/// to subscribe, which dials the venue).
fn build_client_source() -> &'static str {
    let src = include_str!("venues.rs");
    let body = &src[src.find("pub fn build_client(").expect("build_client")..];
    &body[..body.find("\n}\n").expect("build_client's closing brace")]
}

/// The arm of `venue`: from its `"<venue>" =>` line to the line at the arms' own indent that opens
/// the next arm, the comment above it, or the fallback.
fn arm_of(body: &'static str, venue: &str) -> &'static str {
    let start = body
        .find(&format!("\n        \"{venue}\" =>"))
        .unwrap_or_else(|| panic!("no `{venue}` arm in build_client"))
        + 1;
    let rest = &body[start..];
    let end = ["\n        \"", "\n        //", "\n        other =>"]
        .into_iter()
        .filter_map(|marker| rest[1..].find(marker).map(|at| at + 1))
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}

fn holder() -> Holder {
    Holder { session: 7, peer: None, scope: Scope::Read, key_id: None }
}

/// A sink that discards everything: these tests look at what is built, not at what it emits.
struct Silent;

impl LiveDataSink for Silent {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
    fn quote(&self, _v: &str, _s: &str, _q: QuoteTick) {}
    fn trade(&self, _v: &str, _s: &str, _t: TradeTick) {}
    fn book(&self, _v: &str, _s: &str, _b: Arc<L2Book>) {}
}

/// A venue client that does nothing: what a scripted table hands the lane.
struct Mute;

impl DataClient for Mute {
    fn subscribe_bars(&mut self, _: &str, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Subscribe("mute".into()))
    }
    fn subscribe_quotes(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Subscribe("mute".into()))
    }
    fn subscribe_trades(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Subscribe("mute".into()))
    }
    fn subscribe_book(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Subscribe("mute".into()))
    }
    fn unsubscribe(&mut self, _: SubscriptionId) {}
    fn shutdown(&mut self) {}
}

/// The arms and the stream roster are the same list, in the same order: a venue the lane advertises
/// has an arm, and an arm exists for no venue the lane does not advertise.
/// `crates/vike-gatewayhub/src/registry/registry_tests.rs`'s `a_registry_venue_has_every_site_it_needs`
/// holds the rosters against the client's, both ways; this is the half that needs the arms.
#[test]
fn the_arms_are_exactly_the_stream_roster_in_order() {
    let arms: Vec<&str> = build_client_source()
        .lines()
        .filter_map(|line| {
            let (venue, rest) = line.strip_prefix("        \"")?.split_once('"')?;
            rest.starts_with(" =>").then_some(venue)
        })
        .collect();
    assert_eq!(arms, roster(Lane::Stream), "build_client's arms and the Stream roster disagree");
}

/// The both-ways check between the stream roster and [`build_client`]'s behaviour: every rostered
/// venue BUILDS, and every other roster venue is refused naming the table — never with a feature to
/// rebuild with. It is what keeps the constructors themselves compiled AND called in the `data` build.
/// (The datahub's `every_client_venue_builds_or_names_its_feature` adapted: with every bridge linked
/// there is no compiled-out venue to name a feature for.)
#[test]
fn every_stream_venue_builds_a_client_and_no_other_venue_does() {
    for venue in roster(Lane::Stream) {
        if let Err(e) = build_client(venue, Arc::new(Silent), &BTreeMap::new()) {
            panic!("the stream venue `{venue}` has no client: {e}");
        }
    }
    for &venue in vike_model::VENUES {
        if roster(Lane::Stream).contains(&venue) {
            continue;
        }
        let Err(e) = build_client(venue, Arc::new(Silent), &BTreeMap::new()) else {
            panic!("`{venue}` builds a stream client but is not in the Stream roster");
        };
        assert!(e.contains("Table:") && !e.contains("--features"), "{e}");
    }
}

/// **Each CEX arm hands its OWN venue's `mark_streams` row to its OWN feed, and the Polymarket
/// arm hands its `ws_tokens_per_socket` row to the shard sizing** (decision 0095).
///
/// ⚠ A STRUCTURAL pin, and why it cannot be a behavioural one: [`build_client`] returns a
/// `Box<dyn DataClient>`, the five feeds keep the mark-stream switch in a private field, and
/// the only way to see it work is to subscribe — which dials the venue. What can be held is the
/// wiring in the arms, so this reads the function's own source. It fails for the two defects the
/// wiring can have — an arm that drops the `.with_mark_streams(…)` call (the row would then be
/// stored, shown by `config show` and read by nothing on the data server), and an arm that reads
/// ANOTHER venue's row (a copy-paste of the bybit arm into okx's) — and for a Polymarket arm that
/// stops sizing its shards from the row. `crates/vike-secrets/src/venue_setting_tests.rs`'s
/// `a_venues_mark_stream_row_is_read_under_that_venue_only` is the lookup's behavioural half.
#[test]
fn every_arm_threads_its_own_venues_settings_into_its_own_feed() {
    let body = build_client_source();
    for venue in ["binance", "bybit", "okx", "aster", "hyperliquid"] {
        let text = arm_of(body, venue);
        // hyperliquid groups its keyless data surface under `feeds/`; the others keep `market_feed` at the root.
        let feed = if venue == "hyperliquid" { "feeds::market_feed" } else { "market_feed" };
        // Aster alone is built with an explicit tier (owner ruling 7(a)); every other arm keeps `new`.
        let constructor = if venue == "aster" { "with_env" } else { "new" };
        assert!(
            text.contains(&format!("vike_{venue}::{feed}::Feeds::{constructor}(sink")),
            "the `{venue}` arm must build `{venue}`'s own feed:\n{text}"
        );
        assert!(
            text.contains(&format!(
                ".with_mark_streams(mark_streams_setting(venue_settings, \"{venue}\"))"
            )),
            "the `{venue}` arm must apply `{venue}`'s own mark_streams row:\n{text}"
        );
    }
    let poly = arm_of(body, "polymarket");
    assert!(
        poly.contains("vike_polymarket::tokens_per_socket(")
            && poly.contains(".get(\"polymarket\")"),
        "the polymarket arm must size its shards from polymarket's own settings rows:\n{poly}"
    );
    assert!(
        poly.contains(".with_tokens_per_socket(k)"),
        "…and hand the result to the feed:\n{poly}"
    );
    // Deribit reads no `venue_setting` row at all, so its arm builds its feed from the sink
    // alone — and must not carry a CEX arm's `mark_streams` wiring (a copy-paste of one).
    let deribit = arm_of(body, "deribit");
    assert!(
        deribit.contains("vike_deribit::market_feed::Feeds::new(sink"),
        "the `deribit` arm must build deribit's own feed:\n{deribit}"
    );
    assert!(
        !deribit.contains("mark_streams"),
        "deribit has no mark_streams row — its marks stream chain-wide:\n{deribit}"
    );
}

/// **Owner ruling 7(a) of decision 0164, the one behaviour change of wave 1: the aster feed is
/// built on MAINNET, and no other arm's network moves.**
///
/// `Feeds::new` defaults the tier to `Demo` (the testnet hosts), which is what the datahub's aster
/// feed was; the arm now hands [`ASTER_FEED_TIER`] to `Feeds::with_env`. The feed keeps its tier in a
/// private field, so two halves are held: the arm passes that constant, and the constant resolves,
/// through the bridge's own host table, to the mainnet PUBLIC market-data hosts and not to the
/// testnet ones. ⚠ The tier names a network, never an account: the arm reads no credential, which is
/// why a box whose aster account is `live` can serve this feed without being asked for its key.
#[test]
fn the_aster_arm_requests_mainnet_and_every_other_arm_keeps_its_network() {
    let body = build_client_source();
    let aster = arm_of(body, "aster");
    assert!(
        aster.contains("vike_aster::market_feed::Feeds::with_env(sink, || {}, ASTER_FEED_TIER)"),
        "the aster arm must build its feed with the explicit mainnet tier:\n{aster}"
    );
    assert!(
        !aster.contains("Feeds::new("),
        "the aster arm fell back to the Demo default:\n{aster}"
    );
    assert_eq!(ASTER_FEED_TIER, AccountTier::Live);
    let live = vike_aster::urls::urls_for(ASTER_FEED_TIER);
    assert_eq!(live.fapi_rest, "https://fapi.asterdex.com");
    assert_eq!(live.sapi_rest, "https://sapi.asterdex.com");
    assert_eq!(live.fapi_ws, "wss://fstream.asterdex.com");
    assert_eq!(live.sapi_ws, "wss://sstream.asterdex.com");
    assert_ne!(
        live,
        vike_aster::urls::urls_for(AccountTier::Demo),
        "the feed tier resolves to the testnet hosts"
    );
    // The arm names no credential, store or signer: the only thing it reads is the `mark_streams` row.
    let lowered = aster.to_lowercase();
    for word in ["credential", "secret", "sign"] {
        assert!(!lowered.contains(word), "the aster arm reaches for `{word}`:\n{aster}");
    }
    // Every other arm keeps the network its constructor defaults to: no tier is passed at all.
    for venue in roster(Lane::Stream) {
        if *venue == "aster" {
            continue;
        }
        let text = arm_of(body, venue);
        assert!(
            text.contains("::Feeds::new(sink") && !text.contains("with_env"),
            "the `{venue}` arm must keep its constructor's default network:\n{text}"
        );
        assert!(!text.contains("AccountTier"), "the `{venue}` arm names a tier:\n{text}");
    }
}

/// An UNKNOWN venue is refused naming the table — never a feature that does not exist.
#[test]
fn an_unknown_venue_names_the_table_not_a_feature() {
    let err = match build_client("kalshi", Arc::new(Silent), &BTreeMap::new()) {
        Ok(_) => panic!("expected an error"),
        Err(e) => e,
    };
    assert!(err.contains("kalshi") && err.contains("Table:"), "{err}");
    assert!(!err.contains("--features"), "{err}");
    assert!(
        err.contains(&format!("Table: [{}]", roster(Lane::Stream).join(", "))),
        "the refusal must name the stream roster, in order: {err}"
    );
}

/// The lane advertises the stream roster whatever its table holds, and opens through the table it
/// was given: the table's client comes back, and its refusal reaches the caller word for word.
#[test]
fn the_lane_advertises_the_roster_and_opens_through_its_table() {
    let asked = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&asked);
    let lane = BridgeData::over(Box::new(
        move |venue: &str,
              _sink: Arc<dyn LiveDataSink>|
              -> Result<Box<dyn DataClient + Send>, String> {
            seen.lock().unwrap_or_else(PoisonError::into_inner).push(venue.to_string());
            if venue == "okx" {
                return Err("scripted: okx refused".to_string());
            }
            Ok(Box::new(Mute))
        },
    ));
    assert_eq!(lane.venues(), owned(Lane::Stream));
    assert!(lane.open("binance", &holder(), Arc::new(Silent)).is_ok());
    match lane.open("okx", &holder(), Arc::new(Silent)) {
        Ok(_) => panic!("the scripted table refused okx"),
        Err(text) => assert_eq!(text, "scripted: okx refused"),
    }
    assert_eq!(*asked.lock().unwrap_or_else(PoisonError::into_inner), ["binance", "okx"]);
}

/// The production lane serves exactly the stream roster, builds each venue's REAL client without
/// dialling anything, and refuses a venue outside the roster naming it.
#[test]
fn the_production_lane_builds_every_rostered_venue_and_refuses_the_rest() {
    let lane = BridgeData::new(BTreeMap::new());
    assert_eq!(lane.venues(), owned(Lane::Stream));
    for venue in lane.venues() {
        assert!(
            lane.open(&venue, &holder(), Arc::new(Silent)).is_ok(),
            "the production lane cannot open `{venue}`"
        );
    }
    match lane.open("ibkr", &holder(), Arc::new(Silent)) {
        Ok(_) => panic!("a single-session venue has no stream client in this daemon"),
        Err(text) => assert!(text.contains("ibkr") && text.contains("Table:"), "{text}"),
    }
}
