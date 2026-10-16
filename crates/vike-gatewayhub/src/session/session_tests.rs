use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
fn a_memo_reads_once_and_keeps_the_answer() {
    let memo: ConnectionMemo<String, &'static str> = ConnectionMemo::new();
    assert!(!memo.is_read());
    let reads = AtomicUsize::new(0);
    for _ in 0..5 {
        let got = memo.get_or_read(|| {
            reads.fetch_add(1, Ordering::SeqCst);
            Ok("token-a".to_string())
        });
        assert_eq!(got.as_deref(), Ok("token-a"));
    }
    assert_eq!(reads.load(Ordering::SeqCst), 1, "the read ran more than once");
    assert!(memo.is_read());
}

#[test]
fn an_unreadable_answer_is_kept_too() {
    // An unreadable store is reported once per connection, not once per day chunk.
    let memo: ConnectionMemo<String, &'static str> = ConnectionMemo::new();
    let reads = AtomicUsize::new(0);
    for _ in 0..3 {
        let got = memo.get_or_read(|| {
            reads.fetch_add(1, Ordering::SeqCst);
            Err("store unreadable")
        });
        assert_eq!(got.as_ref().err(), Some(&"store unreadable"));
    }
    assert_eq!(reads.load(Ordering::SeqCst), 1);
}

#[test]
fn two_scopes_never_share_a_memo_and_a_scope_returns_the_same_slot() {
    type Memo = ConnectionMemo<String, String>;
    let a = ConnectionScope::new();
    let b = ConnectionScope::new();
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
    let scope = ConnectionScope::new();
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
fn a_credential_value_never_reaches_debug() {
    let memo: ConnectionMemo<Redacted<String>, String> = ConnectionMemo::new();
    assert_eq!(format!("{memo:?}"), "ConnectionMemo(unread)");
    let got = memo.get_or_read(|| Ok(Redacted::new("s3cr3t-practice-token".to_string())));
    let rendered = format!("{memo:?} {got:?} {:?}", Redacted::new("s3cr3t-practice-token"));
    assert!(!rendered.contains("s3cr3t"), "a value reached Debug: {rendered}");
    assert!(rendered.contains("ConnectionMemo(ok)"), "{rendered}");
    assert!(rendered.contains("Redacted(***)"), "{rendered}");
    // The scope prints a count, not its slots.
    let scope = ConnectionScope::new();
    let slot = scope.slot::<ConnectionMemo<Redacted<String>, String>>();
    let _ = slot.get_or_read(|| Ok(Redacted::new("s3cr3t-practice-token".to_string())));
    let rendered = format!("{scope:?}");
    assert_eq!(rendered, "ConnectionScope(1 slots)");
    // The deliberate end of redaction is the named accessor, and only it.
    let held = Redacted::new("s3cr3t-practice-token".to_string());
    assert_eq!(held.expose(), "s3cr3t-practice-token");
}

#[test]
fn concurrent_askers_share_one_read() {
    let memo = Arc::new(ConnectionMemo::<u64, String>::new());
    let reads = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let memo = Arc::clone(&memo);
            let reads = Arc::clone(&reads);
            std::thread::Builder::new()
                .name(format!("memo-test-{i}"))
                .spawn(move || {
                    let got = memo.get_or_read(|| {
                        reads.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        Ok(7)
                    });
                    *got.as_ref().expect("the read succeeded")
                })
                .expect("spawn")
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().expect("join"), 7);
    }
    assert_eq!(reads.load(Ordering::SeqCst), 1);
}
