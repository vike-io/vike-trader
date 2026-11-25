use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, RandomState};

use super::*;
use crate::{Bar, TickQuote, TickTrade};

/// A handle made OUTSIDE this process's table, as a strategy file's own copy of this crate makes one: another
/// entry of the same text, its hash computed by the same function.
fn other_table(text: &str) -> Symbol {
    let text: &'static str = Box::leak(Box::from(text));
    Symbol(Box::leak(Box::new(Entry { hash: text_hash(text), text })))
}

#[test]
fn one_text_is_one_handle_in_one_table() {
    let a = Symbol::new("BTCUSDT.P");
    let b = Symbol::new(&String::from("BTCUSDT.P"));
    assert!(std::ptr::eq(a.0, b.0), "interned once");
    let c = Symbol::new("ETHUSDT.P");
    // Another text in between: the thread's last hit moved, its own map still answers the first handle.
    assert!(std::ptr::eq(Symbol::new("BTCUSDT.P").0, a.0));
    assert_ne!(a, c);
    // Another thread meets the same text: its own map is empty, the process table answers the same entry.
    let elsewhere = std::thread::spawn(|| Symbol::new("BTCUSDT.P").0 as *const Entry as usize);
    assert_eq!(elsewhere.join().expect("the thread"), a.0 as *const Entry as usize);
    assert!(
        std::ptr::eq(Symbol::new("").0, Symbol::default().0),
        "the empty symbol takes no entry"
    );
}

/// The stored hash is FNV-1a 64 of the text, seedless: the same value in every process and strategy file.
#[test]
fn the_stored_hash_is_a_fixed_function_of_the_text() {
    assert_eq!(text_hash(""), 0xcbf2_9ce4_8422_2325, "FNV-1a 64's offset basis");
    assert_eq!(text_hash("a"), 0xaf63_dc4c_8601_ec8c, "FNV-1a 64 of \"a\"");
    assert_eq!(text_hash("foobar"), 0x8594_4171_f739_67e8, "FNV-1a 64 of \"foobar\"");
    assert_eq!(Symbol::new("foobar").0.hash, text_hash("foobar"));
    assert_eq!(Symbol::default().0.hash, text_hash(""));
}

#[test]
fn a_handle_of_another_table_is_equal_by_its_text_and_hashes_alike() {
    let host = Symbol::new("EURUSD");
    let guest = other_table("EURUSD");
    assert!(!std::ptr::eq(host.0, guest.0), "two tables, two entries");
    assert_eq!(host, guest);
    assert_eq!(guest, "EURUSD");
    assert_eq!("EURUSD", guest);
    assert_eq!(guest, String::from("EURUSD"));
    assert_ne!(guest, other_table("EURUSD.CFD"));
    assert_ne!(host, Symbol::new("EURUSD.CFD"));
    assert_eq!(host.cmp(&guest), std::cmp::Ordering::Equal);

    let state = RandomState::new();
    assert_eq!(state.hash_one(host), state.hash_one(guest));
    assert_ne!(state.hash_one(host), state.hash_one(Symbol::new("EURUSD.CFD")));
    let set: HashSet<Symbol> = [host].into();
    assert!(set.contains(&guest));
    let map: HashMap<Symbol, u32> = [(guest, 7)].into();
    assert_eq!(map.get(&host), Some(&7));
    assert_eq!(map.get(&Symbol::new("EURUSD")), Some(&7));
}

#[test]
fn it_reads_and_prints_as_its_text() {
    let symbol = Symbol::from("BTC-PERP");
    assert_eq!(symbol.as_str(), "BTC-PERP");
    assert_eq!(&*symbol, "BTC-PERP");
    assert!(symbol.starts_with("BTC"));
    assert_eq!(format!("{symbol}|{symbol:>10}|{symbol:?}"), "BTC-PERP|  BTC-PERP|\"BTC-PERP\"");
    assert_eq!(format!("{:?}", Some(symbol)), format!("{:?}", Some("BTC-PERP".to_string())));
    assert_eq!(String::from(symbol), "BTC-PERP");
    assert!(Symbol::new("A") < Symbol::new("B"));
    assert_eq!(Symbol::new("binance"), "binance", "a venue is a symbol too");
}

/// Serde writes the TEXT, as a `String` does, byte for byte, so no wire or file changes; reading interns.
#[test]
fn serde_writes_the_text_as_a_string_does() {
    for text in
        ["", "BTCUSDT.P", "1234567890123456789012345678901234567890123456789012345678901234567890"]
    {
        let symbol = Symbol::new(text);
        let json = serde_json::to_string(&symbol).unwrap();
        assert_eq!(json, serde_json::to_string(text).unwrap());
        let back: Symbol = serde_json::from_str(&json).unwrap();
        assert_eq!(back, text);
        assert!(std::ptr::eq(back.0, symbol.0), "reading interns: the same handle");
        assert_eq!(
            serde_json::to_string(&Some(symbol)).unwrap(),
            serde_json::to_string(&Some(text.to_string())).unwrap()
        );
    }
    let none: Option<Symbol> = serde_json::from_str("null").unwrap();
    assert_eq!(none, None);
    assert!(serde_json::from_str::<Symbol>("7").is_err(), "a number is not a symbol");
}

/// A peer's text longer than `MAX_READ_LEN` is a malformed reply: refused, and never interned (a table entry
/// is kept for the process's life). The longest real name, a Polymarket token id, reads.
#[test]
fn a_text_past_the_read_cap_is_refused_and_never_interned() {
    let token = "7".repeat(78);
    let read: Symbol = serde_json::from_str(&format!("\"{token}\"")).unwrap();
    assert_eq!(read, token.as_str());
    let longest = "a".repeat(Symbol::MAX_READ_LEN);
    assert!(serde_json::from_str::<Symbol>(&format!("\"{longest}\"")).is_ok());
    let past = "b".repeat(Symbol::MAX_READ_LEN + 1);
    let refused = serde_json::from_str::<Symbol>(&format!("\"{past}\"")).expect_err("past the cap");
    assert!(refused.to_string().contains("at most 256 bytes"), "{refused}");
    assert!(!Symbol::is_interned(&past), "a refused text takes no entry");
    assert!(Symbol::is_interned(&longest));
}

/// The rows' bytes in memory: one pointer where a `String` was (8 bytes for 24), and `Option<Symbol>` too.
#[cfg(target_pointer_width = "64")]
#[test]
fn a_symbol_is_one_pointer_and_the_rows_shrink() {
    use std::mem::size_of;
    assert_eq!(size_of::<Symbol>(), 8);
    assert_eq!(size_of::<Option<Symbol>>(), 8);
    assert_eq!(size_of::<Bar>(), 104);
    assert_eq!(size_of::<TickTrade>(), 48);
    assert_eq!(size_of::<TickQuote>(), 56);
}

/// The rows' serde form is the one they had with `String` symbols: the same JSON for a bar and a tick.
#[test]
fn a_row_serializes_as_it_did_with_a_string_symbol() {
    let trade = TickTrade {
        timestamp: 1,
        local_timestamp: 2,
        price: 3.5,
        size: 0.25,
        is_buyer_maker: true,
        symbol: "BTCUSDT.P".into(),
    };
    assert_eq!(
        serde_json::to_string(&trade).unwrap(),
        r#"{"timestamp":1,"local_timestamp":2,"price":3.5,"size":0.25,"is_buyer_maker":true,"symbol":"BTCUSDT.P"}"#
    );
    let mut bar = Bar::ohlcv(0, 1.0, 2.0, 0.5, 1.5, 10.0);
    assert!(serde_json::to_string(&bar).unwrap().ends_with(r#""symbol":null}"#));
    bar.symbol = Some("EURUSD".into());
    let json = serde_json::to_string(&bar).unwrap();
    assert!(json.ends_with(r#""symbol":"EURUSD"}"#), "{json}");
    assert_eq!(serde_json::from_str::<Bar>(&json).unwrap(), bar);
    // A tick written without a symbol reads as the empty one.
    let quote: TickQuote = serde_json::from_str(r#"{"timestamp":1,"bid":1.0,"ask":2.0}"#).unwrap();
    assert_eq!(quote.symbol, "");
}
