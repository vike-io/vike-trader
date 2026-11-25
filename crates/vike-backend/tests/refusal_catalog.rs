//! **`refusals.json`, the workspace's refusal catalog** (refusal catalog step 4): every refusal id this workspace
//! declares, its ONE text template, the crate that declares it and, where code decides by it, the outcome of the
//! refused call. RENDERED here from each refusal enum's `ALL` (`vike_utility::refusal_enum!`), never written by hand.
//!
//! **Rendered in this crate** because only `vike-backend` (layer 68) links every crate that refuses: the engine, the
//! core, the hubs' clients and the CLI; `vike-docs` at 25 and `vike-mcp` at 35 cannot. The committed copy is
//! `crates/vike-mcp/src/refusals.json`: the MCP server compiles it in (`vike_mcp::REFUSAL_CATALOG`, served as
//! `vike://refusals`) and the release copies it beside `cli.json` (`.github/workflows/release.yml`'s docs-data step).
//!
//! What these tests hold:
//! - the copy IS the render, byte for byte (`the_committed_catalog_is_the_render`, which prints the render when the
//!   two differ);
//! - an id once published never leaves: one its enum drops keeps a row in [`RETIRED`] and stays published,
//!   `"retired": true`, so an id is never renamed or reused (`no_published_id_ever_leaves_the_catalog`);
//! - every `refusal_enum!` a production file declares is walked by [`declared`] (`every_refusal_enum_is_walked`);
//! - no id is declared twice, in one enum or across two ([`render`]).
//!
//! The writer, in a lane, then READ THE DIFF:
//! `cargo test -p vike-backend --test integration refusal_catalog::bless_the_committed_catalog -- --ignored`.
//!
//! Whole-file gated on the `vike-cli`, `vike-gatewayhub` and `vike-telegram` features, the three optional edges it
//! names (`vike_cli::refusals::FlagRefusalId`, `vike_gatewayhub::refusals::UrlProbeRefusalId`,
//! `vike_telegram::TelegramRefusalId`); the crate's self dev-dependency turns `full`, and so all three, on in every
//! test build.
#![cfg(all(feature = "vike-cli", feature = "vike-gatewayhub", feature = "vike-telegram"))]

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Map, Value, json};
use vike_hub_protocol::refusal::HubRefusal;
use vike_tradehub_client::refusals::{TradehubRefusalId, frame_outcome};
use vike_utility::errors::FailureOutcome;

/// One refusal enum's ids: the id enum's path (its crate and its name), and each id's `word()` and `template()`.
struct Declared {
    path: &'static str,
    ids: Vec<(&'static str, &'static str)>,
}

fn ids_of<I: Copy + 'static>(
    all: &[I],
    word: fn(I) -> &'static str,
    template: fn(I) -> &'static str,
) -> Declared {
    Declared {
        path: std::any::type_name::<I>(),
        ids: all.iter().map(|&id| (word(id), template(id))).collect(),
    }
}

/// `ids_of` over each id enum's own `ALL`, `word` and `template`.
macro_rules! walk {
    ($($id:path),+ $(,)?) => { vec![$(ids_of(&<$id>::ALL, <$id>::word, <$id>::template)),+] };
}

/// Every refusal enum of the workspace, by its id enum. A new `refusal_enum!` is a line here:
/// `every_refusal_enum_is_walked` fails without it.
fn declared() -> Vec<Declared> {
    walk![
        vike_alpaca::refusals::AlpacaCatalogRefusalId,
        vike_alpaca::refusals::AlpacaFeedRefusalId,
        vike_alpaca::refusals::AlpacaNameRefusalId,
        vike_alpaca::refusals::AlpacaPropertiesRefusalId,
        vike_alpaca::refusals::AlpacaReconcileRefusalId,
        vike_alpaca::refusals::AlpacaStartRefusalId,
        vike_alpaca::refusals::AlpacaStreamRefusalId,
        vike_alpaca::refusals::AlpacaVenueReadRefusalId,
        vike_aster::refusals::AsterFeedRefusalId,
        vike_backfill::refusals::BackfillBinRefusalId,
        vike_backtest::run::refusals::ProfileRefusalId,
        vike_backtesthub_client::client::DialRefusalId,
        vike_binance::refusals::BinanceAccountRefusalId,
        vike_binance::refusals::BinanceHistoryRefusalId,
        vike_binance::refusals::BinanceLeverageRefusalId,
        vike_binance::refusals::BinanceListingRefusalId,
        vike_binance::refusals::BinanceModeBlockerId,
        vike_binance::refusals::BinanceModeRefusalId,
        vike_binance::refusals::BinanceNameRefusalId,
        vike_binance::refusals::BinancePropertiesRefusalId,
        vike_bybit::refusals::BybitAccountRefusalId,
        vike_bybit::refusals::BybitHistoryRefusalId,
        vike_bybit::refusals::BybitLeverageRefusalId,
        vike_bybit::refusals::BybitSymbolRefusalId,
        vike_catalog::InstrumentListErrorId,
        vike_catalog::refusals::SessionListRefusalId,
        vike_catalog::refusals::SymbolRefusalId,
        vike_cli::refusals::FlagRefusalId,
        vike_cli::refusals::RetiredConfigSpellingId,
        vike_cli::refusals::RetiredDataSpellingId,
        vike_cli::refusals::RetiredSpellingId,
        vike_cli::refusals::RetiredTradeSpellingId,
        vike_client::refusals::BackgroundRunRefusalId,
        vike_client::refusals::BacktestCallRefusalId,
        vike_client::refusals::DeviceKeyRefusalId,
        vike_client::refusals::ModifyRefusalId,
        vike_client::refusals::NodeAddressRefusalId,
        vike_client::refusals::OrderGateRefusalId,
        vike_client::refusals::OrderListRefusalId,
        vike_client::refusals::ProfileTemplateRefusalId,
        vike_client::refusals::RenamedArgumentRefusalId,
        vike_client::refusals::RenamedToolRefusalId,
        vike_client::refusals::SubmitRuleRefusalId,
        vike_config::refusals::ConfigRefusalId,
        vike_config::refusals::SettingsWriteRefusalId,
        vike_core::CoreOrderRefusalId,
        vike_ctrader::history::CtraderHistoryConfigId,
        vike_ctrader::refusals::CtraderCatalogRefusalId,
        vike_ctrader::refusals::CtraderFeeRefusalId,
        vike_ctrader::refusals::CtraderHistoryRefusalId,
        vike_ctrader::refusals::CtraderNameRefusalId,
        vike_ctrader::refusals::CtraderPropertiesRefusalId,
        vike_ctrader::refusals::CtraderRateRefusalId,
        vike_ctrader::refusals::CtraderReconcileRefusalId,
        vike_ctrader::refusals::CtraderStartRefusalId,
        vike_ctrader::refusals::CtraderVenueReadRefusalId,
        vike_data::store::removal::RemovalRefusalId,
        vike_data::store::removal::SelectorRefusalId,
        vike_datahub::recorder::RecorderProfileRefusalId,
        vike_datahub_client::client::data_ops::DeleteRefusalId,
        vike_datahub_client::refusals::DatahubRefusalId,
        vike_deribit::refusals::DeribitClockRefusalId,
        vike_deribit::refusals::DeribitContractRefusalId,
        vike_deribit::refusals::DeribitFeedRefusalId,
        vike_deribit::refusals::DeribitHistoryRefusalId,
        vike_deribit::refusals::DeribitNameRefusalId,
        vike_deribit::refusals::DeribitOptionsRefusalId,
        vike_deribit::refusals::DeribitPropertiesRefusalId,
        vike_deribit::refusals::DeribitReconcileRefusalId,
        vike_deribit::refusals::DeribitVenueReadRefusalId,
        vike_dukascopy::DukascopyRefusalId,
        vike_dukascopy::refusals::DukascopyHistoryRefusalId,
        vike_dukascopy::refusals::DukascopyPriceLaneRefusalId,
        vike_dukascopy::refusals::DukascopyPropertiesRefusalId,
        vike_dukascopy::refusals::DukascopyRateRefusalId,
        vike_dukascopy::refusals::DukascopyReconcileRefusalId,
        vike_dukascopy::refusals::DukascopyStartRefusalId,
        vike_fills::refusals::CostRefusalId,
        vike_fxcm::refusals::FxcmArchiveFileRefusalId,
        vike_fxcm::refusals::FxcmCatalogRefusalId,
        vike_fxcm::refusals::FxcmHistoryRefusalId,
        vike_fxcm::refusals::FxcmPriceRefusalId,
        vike_fxcm::refusals::FxcmSessionErrorId,
        vike_fxcm::refusals::FxcmSubscribeRefusalId,
        vike_fxcm::refusals::FxcmSymbolRefusalId,
        vike_fxcm::refusals::FxcmVenueReadRefusalId,
        vike_gatewayhub::refusals::UrlProbeRefusalId,
        vike_gatewayhub_client::refusals::GatewayhubRefusalId,
        vike_gatewayhub_client::refusals::GatewayhubTradeRefusalId,
        vike_hub_protocol::call::HubAccessRefusalId,
        vike_hyperliquid::refusals::HyperliquidAccountRefusalId,
        vike_hyperliquid::refusals::HyperliquidCatalogRefusalId,
        vike_hyperliquid::refusals::HyperliquidFeedRefusalId,
        vike_hyperliquid::refusals::HyperliquidHistoryRefusalId,
        vike_hyperliquid::refusals::HyperliquidLeverageRefusalId,
        vike_hyperliquid::refusals::HyperliquidPropertiesRefusalId,
        vike_hyperliquid::refusals::HyperliquidReconcileRefusalId,
        vike_hyperliquid::refusals::HyperliquidVenueReadRefusalId,
        vike_ibkr::refusals::IbkrConfigRefusalId,
        vike_ibkr::refusals::IbkrReadErrorId,
        vike_ibkr::refusals::IbkrSymbolRefusalId,
        vike_ibkr::refusals::IbkrVenueReadRefusalId,
        vike_ig::refusals::IgHistoryRefusalId,
        vike_ig::refusals::IgLoginRefusalId,
        vike_ig::refusals::IgPricesErrorId,
        vike_ig::refusals::IgRateRefusalId,
        vike_ig::refusals::IgSymbolRefusalId,
        vike_ig::refusals::IgVenueReadRefusalId,
        vike_model::OrderDenialId,
        vike_model::OrderRefusalId,
        vike_oanda::HistoryTokenErrorId,
        vike_oanda::refusals::OandaCandleRefusalId,
        vike_oanda::refusals::OandaCatalogRefusalId,
        vike_oanda::refusals::OandaFeedRefusalId,
        vike_oanda::refusals::OandaHistoryRefusalId,
        vike_oanda::refusals::OandaPropertiesRefusalId,
        vike_oanda::refusals::OandaRateRefusalId,
        vike_oanda::refusals::OandaReconcileRefusalId,
        vike_oanda::refusals::OandaSeedRefusalId,
        vike_oanda::refusals::OandaVenueReadRefusalId,
        vike_okx::refusals::OkxAccountRefusalId,
        vike_okx::refusals::OkxHistoryRefusalId,
        vike_okx::refusals::OkxInstrumentRefusalId,
        vike_okx::refusals::OkxLeverageRefusalId,
        vike_okx::refusals::OkxModeBlockerId,
        vike_okx::refusals::OkxModeRefusalId,
        vike_okx::refusals::OkxMountRefusalId,
        vike_okx::refusals::OkxPropertiesRefusalId,
        vike_polymarket::refusals::PolymarketChainRefusalId,
        vike_polymarket::refusals::PolymarketEgressRefusalId,
        vike_polymarket::refusals::PolymarketL2RefusalId,
        vike_polymarket::refusals::PolymarketMarketRefRefusalId,
        vike_polymarket::refusals::PolymarketPropertiesRefusalId,
        vike_polymarket::refusals::PolymarketReadRefusalId,
        vike_polymarket::refusals::PolymarketRedeemRefusalId,
        vike_polymarket::refusals::PolymarketRulesRefusalId,
        vike_polymarket::refusals::PolymarketStartRefusalId,
        vike_polymarket::refusals::PolymarketVenueReadRefusalId,
        vike_strategy_builder::builder::KeyRefusalId,
        vike_strategy_builder::builder::StartRefusalId,
        vike_strategy_builder::client::BuildRequestErrorId,
        vike_strategy_builder::render::BuildErrorId,
        vike_strategy_sandbox::refusals::SandboxRefusalId,
        vike_telegram::TelegramRefusalId,
        vike_tradehub::refusals::AccountVerbRefusalId,
        vike_tradehub::refusals::CommandRefusalId,
        vike_tradehub::refusals::DoorRefusalId,
        vike_tradehub::refusals::LimitRefusalId,
        vike_tradehub::refusals::MountFileRefusalId,
        vike_tradehub::refusals::MountProfileRefusalId,
        vike_tradehub::refusals::MountVerbRefusalId,
        vike_tradehub::refusals::NotRunningReasonId,
        vike_tradehub::refusals::SessionRefusalId,
        vike_tradehub::refusals::SettingsRefusalId,
        vike_tradehub::refusals::SymbolRefusalId,
        vike_tradehub::refusals::VenueReadRefusalId,
        TradehubRefusalId,
    ]
}

/// Every id a refusal enum declared once and declares no more, `(id, template, crate)`: an id is never renamed,
/// reused or dropped, so one that leaves its enum keeps its row here and the catalog goes on publishing it,
/// `"retired": true`. None has left yet.
const RETIRED: &[(&str, &str, &str)] = &[];

/// The crate an id enum's path names: its first segment, as the package spells it.
fn crate_of(path: &str) -> String {
    path.split("::").next().unwrap_or_default().replace('_', "-")
}

/// The id enum's own name: its path's last segment.
fn name_of(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or_default()
}

/// What the refused call DID, for the ids whose outcome code decides by: the tradehub's frames
/// (`vike_tradehub_client::refusals::frame_outcome`), and the one order gate's, every one NOT SENT (nothing left the
/// box: `vike_client::order_gate`'s module doc). `None` for every other id: no code states its outcome yet.
fn category_of(id: &str) -> Option<&'static str> {
    if vike_client::refusals::OrderGateRefusalId::from_word(id).is_some() {
        return Some("not_sent");
    }
    TradehubRefusalId::from_word(id)?;
    Some(match frame_outcome(&HubRefusal::new(id, "")) {
        FailureOutcome::NotSent => "not_sent",
        FailureOutcome::OutcomeUnknown => "outcome_unknown",
        FailureOutcome::NotNow => "not_now",
        FailureOutcome::Refused => "refused",
    })
}

/// `refusals.json`: one row per id, live and retired, sorted by id, each `{category?, crate, id, retired?, template}`.
/// Panics on an id declared twice. Ends with a newline, like every other text asset this repo publishes.
fn render() -> String {
    let live = declared().into_iter().flat_map(|enumeration| {
        let krate = crate_of(enumeration.path);
        enumeration.ids.into_iter().map(move |(id, template)| (id, template, krate.clone(), false))
    });
    let retired =
        RETIRED.iter().map(|&(id, template, krate)| (id, template, krate.to_string(), true));
    let mut rows: Vec<_> = live.chain(retired).collect();
    rows.sort_by_key(|row| row.0);
    for pair in rows.windows(2) {
        assert_ne!(
            pair[0].0, pair[1].0,
            "the id {} is declared twice ({} and {}): an id names ONE refusal",
            pair[0].0, pair[0].2, pair[1].2
        );
    }
    let refusals: Vec<Value> = rows
        .into_iter()
        .map(|(id, template, krate, retired)| {
            let mut row = Map::new();
            if let Some(category) = category_of(id) {
                row.insert("category".into(), category.into());
            }
            row.insert("crate".into(), krate.into());
            row.insert("id".into(), id.into());
            if retired {
                row.insert("retired".into(), true.into());
            }
            row.insert("template".into(), template.into());
            Value::Object(row)
        })
        .collect();
    let document = json!({ "refusals": refusals, "schema_version": 1 });
    let mut text = serde_json::to_string_pretty(&document).expect("a JSON value renders");
    text.push('\n');
    text
}

/// The ids of a catalog document.
fn ids_in(document: &str) -> BTreeSet<String> {
    let parsed: Value = serde_json::from_str(document).expect("a catalog is JSON");
    parsed["refusals"]
        .as_array()
        .expect("a catalog carries `refusals`")
        .iter()
        .map(|row| row["id"].as_str().expect("every row has an id").to_string())
        .collect()
}

/// The committed copy is the render: what the MCP server serves and the release ships is what the enums declare.
#[test]
fn the_committed_catalog_is_the_render() {
    let render = render();
    if vike_mcp::REFUSAL_CATALOG != render {
        println!("=== the render of refusals.json ===\n{render}=== end of the render ===");
    }
    assert!(
        vike_mcp::REFUSAL_CATALOG == render,
        "crates/vike-mcp/src/refusals.json is not the render of the refusal enums (printed above): re-render it with \
         `bless_the_committed_catalog` (this module's doc), read the diff and commit it"
    );
}

/// An id once published never leaves the catalog: every id of the committed copy is still declared, or retired.
#[test]
fn no_published_id_ever_leaves_the_catalog() {
    let rendered = ids_in(&render());
    let left: Vec<String> =
        ids_in(vike_mcp::REFUSAL_CATALOG).difference(&rendered).cloned().collect();
    assert!(
        left.is_empty(),
        "published ids no refusal enum declares and RETIRED does not list: {left:?}. An id is never renamed, reused \
         or dropped: put each back, or give it a RETIRED row with its last template and its crate"
    );
}

/// Every id enum a PRODUCTION file declares through `refusal_enum!` (`enum X with ids XId {`). Test code is left out:
/// a `tests/` directory, a `tests.rs` or `*_tests.rs` file; so is a comment line and the macro's own `$id` pattern.
fn declared_in_source() -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut directories = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("..")];
    while let Some(directory) = directories.pop() {
        let entries = std::fs::read_dir(&directory)
            .unwrap_or_else(|e| panic!("cannot list {}: {e}", directory.display()));
        for entry in entries {
            let path = entry.expect("a directory entry").path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
            if path.is_dir() {
                if !matches!(name.as_str(), "tests" | "target" | "node_modules") {
                    directories.push(path);
                }
                continue;
            }
            if !name.ends_with(".rs") || name == "tests.rs" || name.ends_with("_tests.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            for line in text.lines().map(str::trim_start).filter(|line| !line.starts_with("//")) {
                if let Some((head, tail)) = line.split_once(" with ids ")
                    && head.contains("enum ")
                {
                    let id: String = tail
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    if !id.is_empty() {
                        found.insert(id);
                    }
                }
            }
        }
    }
    found
}

/// Both ways: every refusal enum the workspace declares is walked, and every walked one is found by the scan (a scan
/// that found nothing would pass the first half alone).
#[test]
fn every_refusal_enum_is_walked() {
    let walked: BTreeSet<String> =
        declared().iter().map(|enumeration| name_of(enumeration.path).to_string()).collect();
    assert_eq!(
        declared_in_source(),
        walked,
        "the id enums `refusal_enum!` declares (left) and the ones `declared` walks (right) differ: a new refusal \
         enum is a line in `declared`"
    );
}

/// **THE WRITER**: re-render `crates/vike-mcp/src/refusals.json` from the enums, in place, refusing first when a
/// published id would leave. `#[ignore]`d, because it writes into the source tree.
#[test]
#[ignore = "writes into the source tree — run it deliberately, then read the diff"]
fn bless_the_committed_catalog() {
    no_published_id_ever_leaves_the_catalog();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../vike-mcp/src/refusals.json");
    let render = render();
    std::fs::write(path, &render).unwrap_or_else(|e| panic!("could not write {path}: {e}"));
    println!("=== the render of refusals.json ===\n{render}=== end of the render ===");
}
