use std::sync::atomic::{AtomicBool, Ordering};

use vike_bridge_core::connection_memo::{ConnectionMemo, Redacted};

use super::*;

/// A credential-shaped value whose drop is observable.
struct Token {
    dropped: Arc<AtomicBool>,
}

impl Drop for Token {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

#[test]
fn two_scopes_never_share_a_memo_and_a_scope_returns_the_same_slot() {
    type Memo = ConnectionMemo<String, String>;
    let a = ConnectionMemos::new();
    let b = ConnectionMemos::new();
    assert!(a.is_empty());
    let slot_a = a.slot::<Memo>();
    assert!(Arc::ptr_eq(&slot_a, &a.slot::<Memo>()), "one scope, two slots of one type");
    assert!(!Arc::ptr_eq(&slot_a, &b.slot::<Memo>()), "two scopes share a slot");
    assert_eq!(a.len(), 1);
    // A different type is a different slot.
    let _other = a.slot::<ConnectionMemo<u32, u32>>();
    assert_eq!(a.len(), 2);
}

#[test]
fn the_value_is_dropped_with_the_scope() {
    type Memo = ConnectionMemo<Token, String>;
    let dropped = Arc::new(AtomicBool::new(false));
    let scope = ConnectionMemos::new();
    {
        let slot = scope.slot::<Memo>();
        let flag = Arc::clone(&dropped);
        let got = slot.get_or_read(|| Ok(Token { dropped: flag }));
        assert!(got.is_ok());
    }
    assert!(!dropped.load(Ordering::SeqCst), "dropped while the connection was still open");
    drop(scope);
    assert!(dropped.load(Ordering::SeqCst), "the credential outlived the connection that read it");
}

#[test]
fn a_scope_prints_a_count_and_never_what_its_slots_hold() {
    let scope = ConnectionMemos::new();
    let slot = scope.slot::<ConnectionMemo<Redacted<String>, String>>();
    let _ = slot.get_or_read(|| Ok(Redacted::new("s3cr3t-practice-token".to_string())));
    let rendered = format!("{scope:?}");
    assert_eq!(rendered, "ConnectionMemos(1 slots)");
}
