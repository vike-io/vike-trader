use super::*;
use ParamValue::{Float, Int};

/// A range reads as its values: whole numbers stay whole, `high` is in when a step lands on it, a
/// fractional step is rounded to its own decimals, and a range that names nothing is `Some` of nothing
/// (so the row reads as no value) while a list is no range at all.
#[test]
fn a_range_reads_as_its_values() {
    let ints = |v: &[i64]| v.iter().map(|&i| Int(i)).collect::<Vec<_>>();
    assert_eq!(range_values("5..30 step 5"), Some(ints(&[5, 10, 15, 20, 25, 30])));
    assert_eq!(range_values(" 1..3 "), Some(ints(&[1, 2, 3])), "no step: steps by 1");
    assert_eq!(
        range_values("5..28 step 5"),
        Some(ints(&[5, 10, 15, 20, 25])),
        "28 is not landed on"
    );
    assert_eq!(range_values("-2..2 STEP 2"), Some(ints(&[-2, 0, 2])));
    assert_eq!(range_values("0.1..0.3 step 0.1"), Some(vec![Float(0.1), Float(0.2), Float(0.3)]));
    assert_eq!(range_values("0.1..0.7 step 0.1").map(|v| v.len()), Some(7));
    assert_eq!(range_values("1..2 step 0.5"), Some(vec![Float(1.0), Float(1.5), Float(2.0)]));
    // Past 22 decimals no power of ten is exact (`vike_math::powers::power_of_ten`): left as written.
    let tiny = format!("0.{}1", "0".repeat(22));
    assert_eq!(range_values(&format!("{tiny}..{tiny}")), Some(vec![Float(1e-23)]));
    for nothing in ["5..1", "1..10 step 0", "1..10 step -1", "a..b", "1..", "0..1e9 step 1"] {
        assert_eq!(range_values(nothing), Some(Vec::new()), "{nothing}");
    }
    assert_eq!(range_values("5, 10, 20"), None, "a list is not a range");
    assert_eq!(
        axis_values("5, 10, 20", None),
        ints(&[5, 10, 20]),
        "a list reads as parse_axis reads it"
    );
    assert_eq!(
        axis_values("1..3", Some(&Float(1.0))),
        vec![Float(1.0), Float(2.0), Float(3.0)],
        "a range is typed by the base knob as a list is"
    );
}

/// The scan's size: the product of the scanned rows' value counts, in words.
#[test]
fn the_scan_size_is_the_product_of_the_rows_in_words() {
    assert_eq!(scan_size_line(&[6, 5]), "6 × 5 = 30 runs");
    assert_eq!(scan_size_line(&[6, 5, 2]), "6 × 5 × 2 = 60 runs");
    assert_eq!(scan_size_line(&[6]), "6 runs");
    assert_eq!(scan_size_line(&[1]), "1 run");
    assert_eq!(scan_size_line(&[]), "");
    assert_eq!(scan_total(&[usize::MAX, 2]), usize::MAX, "saturates, never wraps");
}

/// The shipped default script's numbers are `const`s, which a scan cannot vary; turned into `param()`s
/// they are discovered with the same defaults, whole numbers stay whole, and the script still compiles.
#[test]
fn constants_become_params_a_scan_can_vary() {
    let src = crate::panes::editor::StudioEditorPane::default().source;
    assert_eq!(fixed_constants(&src), ["FAST", "SLOW", "QTY"]);
    assert_eq!(declared_params(&src), Some(Vec::new()), "a const is no parameter");
    let rewritten = constants_as_params(&src);
    assert!(rewritten.contains("const FAST = param(\"FAST\", 5).to_int();"), "{rewritten}");
    assert!(rewritten.contains("const QTY = param(\"QTY\", 1.0);"), "{rewritten}");
    assert!(fixed_constants(&rewritten).is_empty());
    let found = declared_params(&rewritten).expect("the rewritten script compiles");
    let want = [("FAST", "5"), ("SLOW", "20"), ("QTY", "1")];
    assert_eq!(found, want.map(|(n, d)| (n.to_string(), d.to_string())));
    let indented = "fn on_bar() {\n    const X = 3;\n}\n";
    assert!(fixed_constants(indented).is_empty(), "a hook's const is not top level");
}

/// A column-0 `const NAME = 5;` line inside a string literal is text: a backtick string spans lines, a
/// `"` one does past a trailing `\`; a quote in a comment or a character opens no string.
#[test]
fn a_const_line_inside_a_string_is_left_alone() {
    let src = "const A = 1;\nlet doc = `\nconst B = 2;\n`;\nlet s = \"x \\\nconst C = 3;\";\n\
               // don't ` open\nlet q = '`';\nconst D = 4;\n";
    assert_eq!(fixed_constants(src), ["A", "D"]);
    let rewritten = constants_as_params(src);
    assert!(rewritten.contains("\nconst B = 2;\n"), "{rewritten}");
    assert!(rewritten.contains("\nconst C = 3;\""), "{rewritten}");
    assert!(rewritten.contains("const D = param(\"D\", 4).to_int();"), "{rewritten}");
    assert!(rewritten.starts_with("const A = param(\"A\", 1).to_int();\n"), "{rewritten}");
}

/// A strategy file's header names its parameters; only the numeric ones are offered, each with its
/// default, else its range's low end.
#[test]
fn a_header_offers_its_numeric_params() {
    let src = "//! ```toml\n//! [strategy]\n//! [params.fast]\n//! type = \"integer\"\n//! default = 5\n\
               //! [params.size]\n//! type = \"number\"\n//! range = [0.5, 3.0]\n\
               //! [params.symbol]\n//! type = \"text\"\n//! ```\npub fn x() {}\n";
    let found = declared_params(src).expect("the header parses");
    assert_eq!(found, vec![("fast".to_string(), "5".to_string()), ("size".into(), "0.5".into())]);
}

/// Re-cutting the rows to what the strategy declares keeps what the user set by name.
#[test]
fn rows_follow_the_declared_params_and_keep_what_was_set() {
    let kept = StudioParamRow { value: "7".into(), ..StudioParamRow::scanned("fast", "5..9") };
    let gone = StudioParamRow::scanned("old", "1, 2");
    let declared = vec![("slow".into(), "20".into()), ("fast".into(), "5".into())];
    let rows = rows_for(declared, &[kept, gone], &mut Vec::new());
    assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["slow", "fast"]);
    assert_eq!((rows[1].value.as_str(), rows[1].scan, rows[1].range.as_str()), ("7", true, "5..9"));
    assert_eq!(rows[1].default, "5");
}

/// A name edited mid-word (`fast`, `fas`, `fast`: each keystroke compiles) gets its value, tick and range
/// back; once a re-cut declares the names the rows already have, a dropped row is gone for good.
#[test]
fn a_name_edited_mid_word_gets_its_row_back_until_the_names_settle() {
    let set = StudioParamRow { value: "7".into(), ..StudioParamRow::scanned("fast", "5..9") };
    let declare = |name: &str| vec![(name.to_string(), "5".to_string())];
    let mut dropped = Vec::new();
    let rows = rows_for(declare("fas"), std::slice::from_ref(&set), &mut dropped);
    assert_eq!((rows[0].name.as_str(), rows[0].value.as_str()), ("fas", ""));
    let rows = rows_for(declare("fast"), &rows, &mut dropped);
    assert_eq!(rows, [StudioParamRow { default: "5".into(), ..set }], "the row comes back whole");
    let rows = rows_for(declare("fast"), &rows, &mut dropped);
    assert!(dropped.is_empty(), "the names settled");
    let rows = rows_for(declare("slow"), &rows, &mut dropped);
    let rows = rows_for(declare("slow"), &rows, &mut dropped);
    let rows = rows_for(declare("fast"), &rows, &mut dropped);
    assert_eq!(rows[0].value, "", "a name gone past a settled re-cut starts afresh");
}

fn point(fast: i64, slow: i64, sharpe: f64) -> ParamscanPointWire {
    ParamscanPointWire {
        overrides: vec![
            ("fast".into(), Int(fast)),
            ("slow".into(), Int(slow)),
            ("qty".into(), Int(1)),
        ],
        sharpe: Some(sharpe),
        total_return: Some(sharpe / 10.0),
        final_equity: 100.0 + sharpe,
        n_trades: fast as usize,
        max_drawdown: None,
        invalid: None,
    }
}

fn scan() -> ParamscanSummaryWire {
    ParamscanSummaryWire {
        run: "a-1".into(),
        // Ranked best first, as the server answers.
        points: vec![point(10, 20, 2.0), point(5, 20, 1.0), point(10, 30, 0.5), point(5, 30, -1.0)],
        best: None,
        dsr: None,
        pbo: None,
        best_index: 0,
        cost_model: None,
        answered_from_saved_run: String::new(),
    }
}

/// The heatmap spans the two parameters that vary (one held at a value is not offered), each in
/// ascending order, every cell the best point with its pair of values.
#[test]
fn the_heatmap_spans_the_varying_params_and_each_cell_is_its_best_point() {
    let sweep = scan();
    assert_eq!(scanned_names(&sweep), ["fast", "slow"]);
    let (xs, ys, cells) = heatmap_cells(&sweep, "fast", "slow");
    assert_eq!((xs, ys), (vec![Int(5), Int(10)], vec![Int(20), Int(30)]));
    assert_eq!(cells, vec![vec![Some(1), Some(0)], vec![Some(3), Some(2)]]);
    assert_eq!(point_label(&sweep.points[0]), "fast=10, slow=20, qty=1");
}

/// GOLDEN (moved with the scan table from the results pane): a point's label prints a `Float` as an
/// `f64` prints and an `Int` as itself, so a whole float and an integer knob both read `5`, and a
/// fractional value keeps its fraction.
#[test]
fn a_point_label_prints_each_override_as_the_f64_label_did() {
    let point = ParamscanPointWire {
        overrides: vec![
            ("fast".into(), Float(5.0)),
            ("rungs".into(), Int(6)),
            ("size".into(), Float(2.5)),
            ("tiny".into(), Float(1e-7)),
            ("neg".into(), Int(-3)),
        ],
        sharpe: None,
        total_return: None,
        final_equity: 0.0,
        n_trades: 0,
        max_drawdown: None,
        invalid: None,
    };
    assert_eq!(point_label(&point), "fast=5, rungs=6, size=2.5, tiny=0.0000001, neg=-3");
}

/// The table is in the scan's ranked order until a column sorts it; a parameter column sorts by value.
#[test]
fn the_table_sorts_by_any_column() {
    let sweep = scan();
    let names = scanned_names(&sweep);
    assert_eq!(sorted_rows(&sweep, &names, None), [0, 1, 2, 3]);
    assert_eq!(sorted_rows(&sweep, &names, Some((2, false))), [3, 2, 1, 0], "Sharpe rising");
    assert_eq!(sorted_rows(&sweep, &names, Some((0, false))), [1, 3, 0, 2], "fast rising, stable");
    assert_eq!(sorted_rows(&sweep, &names, Some((6, true))), [0, 2, 1, 3], "trades falling");
    assert_eq!(took_text(Duration::from_millis(12_400)), "12.4 seconds");
    assert_eq!(took_text(Duration::from_secs(125)), "2 minutes 5 seconds");
}

/// The maximum drawdown is a column of its own (2026-11-20), sortable, its first click the SHALLOWEST on top; a
/// point from an older server, which sends none, sorts last and reads `—`, and an INVALID point reads INVALID.
#[test]
fn the_table_has_a_drawdown_column_shallowest_first() {
    let mut sweep = scan();
    for (point, dd) in sweep.points.iter_mut().zip([Some(0.30), Some(0.05), None, Some(0.12)]) {
        point.max_drawdown = dd;
    }
    let names = scanned_names(&sweep);
    let drawdown = names.len() + 2;
    assert!(!falls_first(&names, drawdown), "the shallowest drawdown is the best");
    assert!(falls_first(&names, names.len()) && !falls_first(&names, 0));
    assert_eq!(sorted_rows(&sweep, &names, Some((drawdown, false))), [1, 3, 0, 2]);
    assert_eq!(drawdown_cell(&sweep.points[0]), "30.00%");
    assert_eq!(drawdown_cell(&sweep.points[2]), "—", "an older server's point");
    sweep.points[0].invalid = Some("equity reached 0".into());
    assert_eq!(drawdown_cell(&sweep.points[0]), INVALID_LABEL);
}

/// The heatmap outlines the point open in the results pane (its heat fill hides the selected look), and
/// only that one.
#[test]
fn the_heatmap_outlines_the_open_point() {
    let outlined = |shown: Option<usize>| {
        let context = egui::Context::default();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(340.0, 600.0),
            )),
            ..Default::default()
        };
        let mut out = context.run_ui(raw, |ui| {
            scan_results_ui(ui, &scan(), shown);
        });
        out.textures_delta.clear();
        let edge = vike_ui_theme::metrics::stroke::EDGE;
        out.shapes
            .iter()
            .filter(|c| matches!(&c.shape, egui::Shape::Rect(r) if r.stroke.width == edge))
            .count()
    };
    assert_eq!(outlined(Some(2)), 1, "the open point's cell");
    assert_eq!(outlined(None), 0, "no point open, no outline");
}

/// **The table keeps a height of its own at the foot of the panel** (the owner's pass of 2026-11-21: a
/// row and a half showed there, the room left on screen): with only the screen's last 60 points left, a
/// scan of twenty points still takes eight rows' height, and scrolls the rest.
#[test]
fn the_table_keeps_eight_rows_at_the_foot_of_the_panel() {
    let mut sweep = scan();
    // `fast` alone varies: no heatmap above the table.
    sweep.points = (0..20).map(|i| point(i, 20, i as f64)).collect();
    let context = egui::Context::default();
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(340.0, 600.0))),
        ..Default::default()
    };
    let mut used = 0.0;
    let mut out = context.run_ui(raw, |ui| {
        ui.allocate_space(egui::vec2(1.0, 540.0));
        let top = ui.min_rect().bottom();
        scan_results_ui(ui, &sweep, None);
        used = ui.min_rect().bottom() - top;
    });
    out.textures_delta.clear();
    let row = context.global_style().spacing.interact_size.y;
    assert!(used >= 8.0 * row, "the table took {used} points, under eight rows of {row}");
}

/// The results paint and ask for nothing until a point is clicked.
#[test]
fn the_results_ask_nothing_without_a_click() {
    let context = egui::Context::default();
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(340.0, 600.0),
        )),
        ..Default::default()
    };
    let mut asked = Vec::new();
    let mut out = context.run_ui(raw, |ui| asked.push(scan_results_ui(ui, &scan(), Some(0))));
    out.textures_delta.clear();
    vike_ui_theme::frame_sanity::assert_frame_sane(&out);
    assert!(asked.iter().all(Option::is_none), "no click, no point: {asked:?}");
}
