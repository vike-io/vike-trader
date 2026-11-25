//! `vike_model::host_build::row_symbol` on planted files: the reads and writes of a bar's and a tick's
//! symbol the shipped strategies had before the `Symbol` rows (M1 to M3) move back onto the `String` type
//! they read, a read the rule cannot type draws a hint on its line and is not touched, a pattern and
//! another type's variant are left alone, and a second pass finds nothing. Whether the result BUILDS is the
//! builder's lane test (`crates/vike-strategy-builder/tests/build_errors.rs`'s
//! `a_file_reading_a_row_symbol_as_a_string_draws_the_hint_and_its_migration_builds`).

use vike_model::host_build::moved_name::source_hints;
use vike_model::host_build::row_symbol::{symbol_edits, symbol_hint};
use vike_model::host_build::source_edits::{SourceEdits, apply_edits};

/// The file `symbol_edits` turns `source` into.
fn migrated(source: &str) -> String {
    apply_edits(source, &symbol_edits(source).edits)
}

/// The shapes `strategies/funding/funding_capture.rs`, `strategies/trend/buy_hold.rs`,
/// `strategies/portfolio/rotation_top_k.rs` and the shipped test modules had before the `Symbol` rows.
const OLD: &str = r#"use vike_marketdata::{Bar, TickQuote, TickTrade};
use vike_model::{OrderBroker, Strategy};

fn symbol_key(symbol: &Option<String>) -> String {
    symbol.clone().unwrap_or_default()
}

impl<B: OrderBroker> Strategy<B> for Hold {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        let symbol = self.symbol.clone().or_else(|| bar.symbol.clone());
        let key = symbol_key(&bar.symbol);
        let text: &str = bar.symbol.as_deref().unwrap_or("");
    }

    fn on_quote_tick(&mut self, broker: &mut B, q: &vike_marketdata::TickQuote) {
        let symbol: String = self.symbol.clone().unwrap_or_else(|| q.symbol.clone());
    }
}

fn quote(name: &str, symbol: String) -> TickQuote {
    let made = TickQuote { timestamp: 0, local_timestamp: 0, bid: 1.0, ask: 2.0, bid_size: 0.0, ask_size: 0.0, symbol: String::new() };
    let named = TickTrade { timestamp: 0, local_timestamp: 0, price: 1.0, size: 1.0, is_buyer_maker: false, symbol };
    let bar = Bar { symbol: Some(name.to_string()), ..Bar::ohlcv(0, 1.0, 1.0, 1.0, 1.0, 0.0) };
    let none = Bar { symbol: None, ..bar.clone() };
    let done = TickQuote { symbol: "X".into(), ..made.clone() };
    TickQuote { symbol: if name.is_empty() { String::new() } else { name.to_string() }, ..made }
}
"#;

#[test]
fn the_shipped_shapes_move_back_onto_the_string_they_read() {
    let expected = OLD
        .replace(
            "self.symbol.clone().or_else(|| bar.symbol.clone())",
            "self.symbol.clone().or_else(|| bar.symbol.map(String::from))",
        )
        .replace("symbol_key(&bar.symbol)", "symbol_key(&bar.symbol.map(String::from))")
        .replace("unwrap_or_else(|| q.symbol.clone())", "unwrap_or_else(|| q.symbol.to_string())")
        .replace("symbol: String::new() };", "symbol: String::new().into() };")
        .replace(
            "is_buyer_maker: false, symbol };",
            "is_buyer_maker: false, symbol: symbol.into() };",
        )
        .replace(
            "symbol: Some(name.to_string()),",
            "symbol: Some(name.to_string()).map(Into::into),",
        )
        .replace(
            "symbol: if name.is_empty() { String::new() } else { name.to_string() },",
            "symbol: (if name.is_empty() { String::new() } else { name.to_string() }).into(),",
        );
    assert_eq!(migrated(OLD), expected);
    // Untouched: a read that builds as it is, `None`, a value already `.into()`, a parameter's own clone.
    assert!(expected.contains("bar.symbol.as_deref().unwrap_or(\"\")"));
    assert!(expected.contains("Bar { symbol: None, ..bar.clone() }"));
    assert!(expected.contains("TickQuote { symbol: \"X\".into(), ..made.clone() }"));
    assert!(expected.contains("symbol.clone().unwrap_or_default()"));
    assert_eq!(symbol_edits(OLD).hints, Vec::<String>::new());
}

#[test]
fn a_second_pass_finds_nothing() {
    let once = migrated(OLD);
    assert_eq!(symbol_edits(&once), SourceEdits::default());
}

/// A read the rule cannot type is a hint on its line and stays; the strategy's own fields draw none.
#[test]
fn a_read_it_cannot_type_draws_a_hint_and_stays() {
    let source = "fn f(last: Option<Bar>) {\n    let s = self.symbol.clone();\n    let k = last.symbol.clone();\n    \
                  let r = &fill.symbol;\n    let o = self.config.symbol.clone();\n}\n";
    let found = symbol_edits(source);
    assert!(found.edits.is_empty(), "{:?}", found.edits);
    assert_eq!(found.hints.len(), 2, "{:?}", found.hints);
    assert!(
        found.hints[0].starts_with("line 3: `last.symbol.clone()` is left as it is"),
        "{:?}",
        found.hints
    );
    assert!(
        found.hints[1].starts_with("line 4: `&fill.symbol` is left as it is"),
        "{:?}",
        found.hints
    );
    assert!(symbol_hint(source).is_some());
    assert!(source_hints(source).iter().any(|h| h.contains("`vike-cli strategy migrate <file>`")));
}

/// A pattern, a declaration, a function's return type and another type's variant named `Bar` are no row
/// literal; a file that touches no row's symbol draws no hint.
#[test]
fn patterns_declarations_and_other_types_are_left_alone() {
    let source = "struct Bar { symbol: String }\nfn f(b: &Bar) -> Bar {\n    let Bar { symbol, .. } = b;\n    \
                  match m { Message::Bar { symbol: s, .. } => {} }\n    Message::Bar { symbol: s.to_string() };\n    \
                  if let TickTrade { symbol, .. } = t {}\n    b.symbol.is_empty();\n}\n";
    assert_eq!(symbol_edits(source), SourceEdits::default());
    assert_eq!(symbol_hint(source), None);
}
