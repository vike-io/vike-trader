use super::*;
use crate::accounts::TIER_UNKNOWN;
use crate::test_support::mount;

/// Every text a frame of the Strategies section paints.
fn strategy_texts(words: &BackendNodeWords) -> Vec<String> {
    let context = egui::Context::default();
    vike_ui_theme::appearance::install(
        &context,
        &vike_ui_theme::appearance::ThemeAppearance::default(),
    );
    let out = context.run_ui(egui::RawInput::default(), |ui| strategies_section(ui, words));
    let texts = out
        .shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Text(t) => Some(t.galley.text().to_string()),
            _ => None,
        })
        .collect();
    out.drop_without_applying_deltas();
    texts
}

/// ⚠ A demo account's strategy never reads LIVE, whatever its `live` flag says: a row names the
/// book and the tier the account runs at, and NOT RUNNING and a spent budget where they stand.
#[test]
fn a_mount_row_names_its_account_and_real_tier_never_a_bare_live() {
    let mut stopped = mount("bybit", Some("live"));
    stopped.not_running = true;
    let mut spent = mount("bybit#SUB", Some("demo"));
    spent.latched = true;
    let words = BackendNodeWords {
        mounts: vec![mount("bybit#DEMO", Some("demo"))],
        ..BackendNodeWords::default()
    };
    let texts = strategy_texts(&words);
    for said in ["spread_maker", "bybit/DEMO", "BTCUSDT 1m", "DEMO"] {
        assert!(texts.iter().any(|t| t == said), "{said}: {texts:?}");
    }
    assert!(!texts.iter().any(|t| t == "LIVE"), "{texts:?}");
    let texts = strategy_texts(&BackendNodeWords {
        mounts: vec![stopped, spent],
        ..BackendNodeWords::default()
    });
    for said in ["bybit/DEFAULT", "LIVE", LINK_NOT_RUNNING, "bybit/SUB", "BUDGET SPENT"] {
        assert!(texts.iter().any(|t| t == said), "{said}: {texts:?}");
    }
}

/// A node that predates the fields says no tier and no account: unknown, and the bare venue, never
/// the `live` flag read as LIVE and never `DEFAULT`, which it did not say.
#[test]
fn an_older_nodes_mount_row_says_what_it_does_not_know() {
    let words = BackendNodeWords { mounts: vec![mount("", None)], ..BackendNodeWords::default() };
    let texts = strategy_texts(&words);
    for said in ["bybit", TIER_UNKNOWN] {
        assert!(texts.iter().any(|t| t == said), "{said}: {texts:?}");
    }
    assert!(!texts.iter().any(|t| t == "LIVE" || t.contains("DEFAULT")), "{texts:?}");
    assert_eq!(strategy_texts(BackendNodeWords::NONE), [NO_STRATEGIES], "no row, and it says so");
}
