//! Headless render gate for the tick-driven microstructure study panes
//! (`vike_chart::studies` + [`vike_chart::render::render_study`]).
//!
//! The unit tests in `src/studies.rs` pin the tick→bar VALUE mapping; this file pins the
//! RENDER side: a study whose series was produced by the documented sampling path paints
//! end-to-end through the same `egui_plot` sub-pane machinery an indicator oscillator
//! uses, without panicking, in all three modes that matter — a warmed VPIN (trades only),
//! a book-gated study WITH a book, and a book-gated study WITHOUT one (whose empty series
//! must still render cleanly, painting bands and nothing else).
//!
//! Headless mechanics mirror `draw_characterization.rs`: a bare `egui::Context`, a
//! fabricated `RawInput` with an explicit screen rect, and the real `Plot::show` closure —
//! the exact call shape `subpanes.rs` uses for an oscillator pane.

use vike_chart::studies::{get_study, ActiveStudy};
use vike_chart::ChartOptions;
use vike_model::{L2Book, TradeTick};

fn trade(ts: i64, price: f64, size: f64, buyer_maker: bool) -> TradeTick {
    TradeTick { ts, local_ts: 0, price, size, is_buyer_maker: buyer_maker, symbol: String::new() }
}

fn book(bid_q: f64, ask_q: f64) -> L2Book {
    let mut b = L2Book::new(0.5);
    b.apply_snapshot(1, &[(99.0, bid_q), (98.5, bid_q)], &[(101.0, ask_q), (101.5, ask_q)]);
    b
}

/// Paint one study into a real `egui_plot` sub-pane, the way `subpanes.rs` paints an
/// oscillator. Returns the number of paint shapes the frame emitted, so a test can tell
/// "rendered something" from "rendered nothing" without asserting on pixels.
fn paint(study: &ActiveStudy) -> usize {
    let ctx = egui::Context::default();
    let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(640.0, 160.0));
    let opts = ChartOptions::default();
    let mut shapes = 0;
    // Two frames: egui_plot stores its bounds from the previous frame, exactly like the
    // real pane loop.
    for f in 0..2 {
        let raw = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(f as f64 / 60.0),
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| {
            egui_plot::Plot::new("study_pane").height(140.0).show(ui, |p| {
                vike_chart::render::render_study(p, study, 0.0, 32.0, &opts);
            });
        });
        shapes = out.shapes.len();
        // egui 0.36 panics on a dropped `TexturesDelta` holding unapplied deltas; this harness
        // counts shapes and uploads nothing. Cleared BEFORE the assertion below: `clear` touches
        // no shape, while an assertion unwinding past unapplied deltas panics again in the
        // destructor and ABORTS instead of reporting the bad coordinate.
        out.textures_delta.clear();
        // The one shared headless-frame geometry invariant (`vike-ui-theme`, `test-support`):
        // counting shapes proves the pane painted SOMETHING, this proves what it painted is
        // paintable.
        vike_ui_theme::frame_sanity::assert_frame_sane(&out);
    }
    shapes
}

#[test]
fn warmed_vpin_study_paints_its_series() {
    // bucket_volume 10, window 2, aggressor-flag classification.
    let mut s = ActiveStudy::with_params(1, get_study("vpin").unwrap(), vec![10.0, 2.0, 1.0]);
    for i in 0..12 {
        // alternating aggressor sides so the bucket imbalances vary
        s.on_trade(&trade(i, 100.0 + i as f64 * 0.1, 4.0, i % 3 == 0), None);
        s.sync(i as usize + 1, true);
    }
    assert_eq!(s.series().len(), 13, "12 committed samples + one forming tail");
    assert!(s.series().iter().any(|v| v.is_finite()), "the study warmed up: {:?}", s.series());
    assert!(paint(&s) > 0, "a warmed study must emit paint shapes");
}

#[test]
fn book_gated_study_with_a_book_paints() {
    let mut s = ActiveStudy::with_params(2, get_study("book_imbalance").unwrap(), vec![2.0, 1.0]);
    for i in 0..8 {
        s.on_book(i, &book(3.0 + i as f64, 1.0));
        s.sync(i as usize + 1, false);
    }
    assert_eq!(s.series().len(), 8);
    assert!(s.series().iter().all(|v| v.is_finite()));
    assert!(paint(&s) > 0);
}

#[test]
fn book_gated_study_without_a_book_renders_empty_without_panicking() {
    // No `on_book` ever: the pane must render (bands only) and fabricate no values.
    let mut s = ActiveStudy::new(3, get_study("otr").unwrap());
    for i in 0..8 {
        s.on_trade(&trade(i, 100.0, 1.0, false), None); // trades alone must NOT ungate it
        s.sync(i as usize + 1, true);
    }
    assert!(s.is_empty());
    assert!(s.series().is_empty(), "no book ⇒ no series");
    // Still a clean render: the bands paint, the (absent) line does not.
    let _ = paint(&s);
}

#[test]
fn hidden_bands_still_render() {
    let mut s = ActiveStudy::with_params(4, get_study("vpin").unwrap(), vec![10.0, 2.0, 1.0]);
    s.show_bands = false;
    s.on_trade(&trade(1, 100.0, 10.0, false), None);
    s.sync(1, false);
    assert_eq!(s.series(), &[1.0]);
    let _ = paint(&s);
}

#[test]
fn ob_os_fill_path_renders() {
    let mut s = ActiveStudy::with_params(5, get_study("vpin").unwrap(), vec![10.0, 2.0, 1.0]);
    s.show_ob_os_fill = true; // exercises the polygon fill branch of the shared oscillator body
    for i in 0..6 {
        s.on_trade(&trade(i, 100.0, 6.0, i % 2 == 0), None);
        s.sync(i as usize + 1, false);
    }
    assert!(paint(&s) > 0);
}
