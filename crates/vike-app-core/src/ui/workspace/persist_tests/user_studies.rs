//! User indicators, saved and restored by NAME.

use super::*;

// ------------------------------------------------- user indicators, saved by NAME

/// A stand-in for a compiled user prototype: `close * scale`, one declared knob.
#[derive(Clone)]
struct Scaled {
    scale: f64,
    last: f64,
}

impl vike_chart::indicators::Indicator for Scaled {
    fn on_bar(&mut self, bar: &vike_marketdata::Bar) -> Vec<f64> {
        self.last = bar.close * self.scale;
        vec![self.last]
    }
    fn vectorize(&self, bars: &[vike_marketdata::Bar]) -> Vec<Vec<f64>> {
        vec![bars.iter().map(|b| b.close * self.scale).collect()]
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last]
    }
    fn reset(&mut self) {
        self.last = f64::NAN;
    }
    fn name(&self) -> &str {
        "scaled"
    }
}

/// Named once so the fixture's two spellings (installed name / snapshot name) cannot drift.
const USER_STUDY: &str = "ws_scaled_close";
/// A name nothing resolves — the workspace fixture's "the file is gone" case.
const VANISHED_STUDY: &str = "ws_study_that_no_longer_exists";

/// The name of THE user study these tests share, installed on first use.
///
/// `vike_chart::indicators::install_user_studies` is a once-per-process `OnceLock` and this
/// crate's lib tests run as threads in ONE process, so the install sits behind a `Once` — a
/// second bare call would be the `Err` that function exists to return.
fn installed_user_study() -> &'static str {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let meta = vike_chart::indicators::IndicatorMeta::user(
            USER_STUDY,
            "Scaled Close",
            vike_chart::indicators::RenderKind::Overlay,
            vec![vike_chart::indicators::ParamSpec {
                name: "scale",
                default: 2.0,
                min: 0.0,
                max: 10.0,
                step: 0.5,
            }],
            &|raw: &[f64]| {
                Box::new(Scaled { scale: raw.first().copied().unwrap_or(2.0), last: f64::NAN })
            },
        );
        vike_chart::indicators::install_user_studies(vec![meta]).expect("first install");
    });
    USER_STUDY
}

/// A USER indicator is added, persisted and restored exactly like a built-in — by NAME,
/// carrying its params and its per-line paint.
///
/// Non-vacuous: `add_indicator` resolved through `get` before this seam, so on a reverted tree
/// the second assert fails — the study cannot be added at all and there is nothing to capture.
/// The `close * 4` assertion on the rebuilt series proves the restore rebuilt the USER's
/// recurrence through the meta's factory rather than anything from the built-in catalog.
#[test]
fn a_user_indicator_round_trips_through_the_workspace_by_name() {
    let name = installed_user_study();
    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WinState::new("usr", "BTCUSDT", "1m", WinKind::Chart, r);
    w.add_indicator("rsi", &[]); // a built-in neighbour, to prove nothing shifts onto it
    w.add_indicator(name, &[]);
    assert_eq!(w.indicators.len(), 2, "the user study must actually be addable");
    assert_eq!(w.indicators[1].spec.name, name);
    w.indicators[1].set_params(vec![4.0], &[]);
    w.indicators[1].outputs[0].color = egui::Color32::from_rgb(9, 9, 9);

    let cap = capture(std::slice::from_ref(&w), DisplayTz::Local, false);
    assert_eq!(cap.wins[0].indicators[1].name, name, "persisted by NAME, like every study");
    assert_eq!(cap.wins[0].indicators[1].params, vec![4.0]);

    let json = serde_json::to_string(&cap).unwrap();
    let mut rebuilt = apply(&serde_json::from_str::<Workspace>(&json).unwrap());
    assert_eq!(rebuilt[0].indicators.len(), 2);
    {
        let restored = &rebuilt[0].indicators[1];
        assert_eq!(restored.spec.name, name);
        assert!(restored.spec.is_user());
        assert_eq!(restored.params, vec![4.0], "the user knob survived the round trip");
        assert_eq!(restored.outputs[0].color, egui::Color32::from_rgb(9, 9, 9));
    }

    // …and it computes: fold one real bar and read the user prototype's own formula back.
    let bars = [vike_chart::model::Bar { t: 0.0, ot: 0, o: 5.0, h: 5.0, l: 5.0, c: 5.0, v: 1.0 }];
    rebuilt[0].indicators[1].recompute_full(&bars);
    assert_eq!(
        rebuilt[0].indicators[1].outputs[0].series,
        vec![20.0],
        "close 5 * the restored scale 4"
    );
}

/// ⚠ THE DEGRADATION CASE. A workspace saved with a user indicator whose file has since been
/// deleted, renamed or broken must restore the way an unknown BUILT-IN already does: the study
/// is silently skipped, everything else comes back untouched, and nothing panics.
///
/// Non-vacuous in the way that matters — the claim is not the bare "an unknown name is a
/// no-op" (already true). The specific hazard is `apply`'s restore loop: it applies each
/// snapshot entry's params and per-line paint to `indicators.last_mut()`, so a skipped entry
/// that failed to `continue` would write the VANISHED study's `params: [4.0]` and its colour
/// onto the PRECEDING study — silent corruption of a study that did restore. The two asserts
/// on `neighbour` fail if that guard goes; the length assert alone would not notice.
#[test]
fn a_vanished_user_indicator_is_skipped_without_disturbing_its_neighbour() {
    let name = installed_user_study();
    assert!(vike_chart::indicators::get_any(VANISHED_STUDY).is_none(), "precondition");

    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WinState::new("gone", "BTCUSDT", "1m", WinKind::Chart, r);
    w.add_indicator("rsi", &[]);
    w.add_indicator(name, &[]);
    w.indicators[0].set_params(vec![9.0], &[]);
    w.indicators[1].set_params(vec![4.0], &[]);
    w.indicators[1].outputs[0].color = egui::Color32::from_rgb(1, 2, 3);

    let cap = capture(std::slice::from_ref(&w), DisplayTz::Local, false);
    let json = serde_json::to_string(&cap).unwrap();
    // The file is gone: the SAME snapshot, with the study's name no longer resolving.
    let orphaned = json.replace(name, VANISHED_STUDY);
    assert!(orphaned.contains(VANISHED_STUDY), "the fixture must really name the dead study");

    let rebuilt = apply(&serde_json::from_str::<Workspace>(&orphaned).unwrap());
    assert_eq!(rebuilt[0].indicators.len(), 1, "the vanished study is silently dropped");
    let neighbour = &rebuilt[0].indicators[0];
    assert_eq!(neighbour.spec.name, "rsi");
    assert_eq!(neighbour.params, vec![9.0], "the dead study's params must not land here");
    assert_ne!(
        neighbour.outputs[0].color,
        egui::Color32::from_rgb(1, 2, 3),
        "nor its per-line paint"
    );
}
