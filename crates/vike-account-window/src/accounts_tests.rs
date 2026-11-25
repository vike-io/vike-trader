use super::*;
use crate::test_support::venue_block;
use vike_execution::{CoreSnapshot, PositionView, PriceSource, VenueBlock};

/// **A NOT RUNNING row of a venue this build's roster does not have is its own line, `unknown venue kalshi`**
/// (owner ruling 2026-11-12, variant 2B): kept as the node named it, never dropped and never a venue this desktop
/// trades. A known venue reads as itself, and its label as the node sent it.
#[test]
fn an_engineless_row_of_a_venue_this_build_does_not_know_says_so() {
    let row = |venue: &str, label: Option<&str>| vike_execution::ArmingNotRunningView {
        account: vike_model::accounts::account_keys::AccountNamed::from_wire(venue, label),
        tier: None,
        reason: "no credentials".to_string(),
        block: None,
        refusal_id: None,
    };
    let kalshi = row("kalshi", Some("main"));
    assert_eq!(engineless_venue(&kalshi), "unknown venue kalshi");
    assert_eq!(kalshi.account.label_text(), Some("main"));
    assert_eq!(engineless_venue(&row("okx", None)), "okx");
    let mut snapshot = CoreSnapshot::empty("binance", "BTCUSDT");
    snapshot.not_running = vec![kalshi];
    assert_eq!(engineless(&snapshot).count(), 1, "no engine of it, so it is listed");
}

fn position(symbol: &str, mark_source: Option<PriceSource>) -> PositionView {
    PositionView {
        venue: "binance".into(),
        symbol: symbol.into(),
        position_side: vike_model::events::PositionSide::Both,
        size: 1.0,
        average_price: 100.0,
        unrealized: 0.0,
        mark_source,
        leverage: 0.0,
        liquidation_price: 0.0,
        margin_mode: vike_model::MarginMode::Cross,
        isolated_margin: None,
    }
}

/// ⚠ **Over a DECODED frame the unpriced badge lists only the unpriced symbols** (the fix of top layers wave 3):
/// the node now says which source priced each position, so a priced one no longer reads as unpriced after
/// `map_position`; a frame from an older node, which says no source, lists every symbol, as before.
#[test]
fn the_unpriced_tooltip_over_a_decoded_frame_lists_only_the_unpriced_symbols() {
    let wire = |symbol: &str, mark_source: Option<&str>| {
        vike_tradehub_client::observe_decode::map_position(
            &vike_tradehub_client::wire::PositionViewWire {
                venue: "binance".into(),
                symbol: symbol.into(),
                position_side: "BOTH".into(),
                size: 1.0,
                average_price: 100.0,
                unrealized: 0.0,
                leverage: 0.0,
                liquidation_price: 0.0,
                mark_source: mark_source.map(str::to_string),
            },
        )
    };
    let mut snapshot = CoreSnapshot::empty("binance", "BTCUSDT");
    snapshot.portfolio.venues = vec![venue_block(
        "binance",
        vec![wire("BTCUSDT", Some("mark")), wire("ADAUSDT", None), wire("ETHUSDT", Some("bid"))],
    )];
    assert_eq!(unpriced_tooltip(&snapshot, "binance"), "ADAUSDT");
    snapshot.portfolio.venues[0].positions = vec![wire("BTCUSDT", None), wire("ADAUSDT", None)];
    assert_eq!(
        unpriced_tooltip(&snapshot, "binance"),
        "BTCUSDT, ADAUSDT",
        "an older node: as before"
    );
}

#[test]
fn unpriced_tooltip_lists_only_the_positions_with_no_mark_source() {
    let mut snapshot = CoreSnapshot::empty("binance", "BTCUSDT");
    snapshot.portfolio.venues = vec![venue_block(
        "binance",
        vec![
            position("BTCUSDT", Some(PriceSource::LastTrade)),
            position("PEPEUSDT", None),
            position("WIFUSDT", None),
        ],
    )];
    assert_eq!(unpriced_tooltip(&snapshot, "binance"), "PEPEUSDT, WIFUSDT");
}

/// ⚠ Two blocks named `bybit` (a demo and a LIVE account): each key answers with its own block's
/// positions, the second one included.
#[test]
fn unpriced_tooltip_follows_the_route_key_not_the_venue_name() {
    let mut snapshot = CoreSnapshot::empty("bybit", "BTCUSDT");
    let live = VenueBlock {
        route_key: "bybit#LIVE".to_string(),
        ..venue_block("bybit", vec![position("WIFUSDT", None)])
    };
    snapshot.portfolio.venues = vec![venue_block("bybit", vec![position("DOGEUSDT", None)]), live];
    assert_eq!(unpriced_tooltip(&snapshot, "bybit"), "DOGEUSDT");
    assert_eq!(unpriced_tooltip(&snapshot, "bybit#LIVE"), "WIFUSDT");
}

#[test]
fn unpriced_tooltip_falls_back_when_everything_is_priced_or_the_venue_is_absent() {
    let mut snapshot = CoreSnapshot::empty("binance", "BTCUSDT");
    snapshot.portfolio.venues =
        vec![venue_block("binance", vec![position("BTCUSDT", Some(PriceSource::LastTrade))])];
    assert_eq!(unpriced_tooltip(&snapshot, "binance"), "unpriced positions");
    assert_eq!(unpriced_tooltip(&snapshot, "okx"), "unpriced positions");
}

/// ⚠ Each account's way to its venue reads in its own row: lost and adapter-less accounts by name,
/// one the bridge says is up as up, one nothing speaks for as unknown and never as up, a stopped
/// engine as NOT RUNNING whatever the frame's links say, and an account the node lists with no
/// engine at all gets a row of its own.
#[test]
fn the_accounts_table_says_how_each_accounts_link_stands() {
    use vike_tradehub_client::node_words::BackendNodeWordsLostAccount;
    let context = egui::Context::default();
    vike_ui_theme::appearance::install(
        &context,
        &vike_ui_theme::appearance::ThemeAppearance::default(),
    );
    let mut snapshot = CoreSnapshot::empty("binance", "BTCUSDT");
    snapshot.portfolio.venues = Vec::from(
        ["binance", "bybit", "okx", "polymarket"]
            .map(|v| VenueBlock { tier: Some(AccountTier::Demo), ..venue_block(v, Vec::new()) }),
    );
    snapshot.not_running =
        Vec::from(["okx", "deribit"].map(|v| vike_execution::ArmingNotRunningView {
            account: vike_model::accounts::account_keys::AccountNamed::of(
                v,
                vike_model::accounts::account_keys::AccountLabel::Default,
            ),
            tier: None,
            reason: "no credentials".to_string(),
            block: Some(vike_model::ArmingBlock::NoCredentials),
            refusal_id: None,
        }));
    // A tier change: okx is listed under its NEW tier beside its block's OLD one, still NOT RUNNING.
    snapshot.not_running[0].tier = Some(AccountTier::Live);
    let words = BackendNodeWords {
        lost: vec![BackendNodeWordsLostAccount {
            route_key: "bybit".to_string(),
            word: "NO ADAPTER",
            why: "It has none.".to_string(),
        }],
        path_up: vec!["binance".to_string()],
        ..BackendNodeWords::default()
    };
    let model = equity::AccountWindowEquityModel::from_snapshot(&snapshot);
    let out = context
        .run_ui(egui::RawInput::default(), |ui| accounts_section(ui, &snapshot, &model, &words));
    let texts: Vec<(String, f32)> = out
        .shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Text(t) => {
                let at = t.galley.rect.translate(t.pos.to_vec2()).center().y;
                Some((t.galley.text().to_string(), at))
            }
            _ => None,
        })
        .collect();
    out.drop_without_applying_deltas();
    let row_of = |venue: &str| -> Vec<&str> {
        let y = texts.iter().find(|(s, _)| s == venue).expect(venue).1;
        texts.iter().filter(|(_, at)| (at - y).abs() < 4.0).map(|(s, _)| s.as_str()).collect()
    };
    assert!(row_of("binance").contains(&LINK_UP), "{texts:?}");
    assert!(row_of("bybit").contains(&"NO ADAPTER"), "{texts:?}");
    assert!(row_of("okx").contains(&LINK_NOT_RUNNING), "a stopped engine keeps its block");
    assert!(row_of("deribit").contains(&LINK_NOT_RUNNING), "no engine, and still a row");
    assert!(row_of("polymarket").contains(&LINK_UNKNOWN), "never UP without the node's word");
    assert!(!row_of("polymarket").contains(&LINK_UP));
}
