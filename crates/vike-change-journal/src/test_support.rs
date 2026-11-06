//! The no-secret-in-the-ledger assertion the suites that write a credential through a journalled
//! writer share: vike-ctrader's token-rotation suite and vike-gateway-panel's Save suite. Gated
//! `#[cfg(any(test, feature = "test-support"))]`; only those dev edges turn the feature on.

/// The shortest run of a secret a ledger may not carry. Not the whole string: a record that wrote
/// every character but the last would pass a `contains(secret)` check, and half a live grant or a
/// partial API secret is still a secret.
pub const MIN_LEAK_WINDOW: usize = 6;

/// Panic if the ledger text `raw` carries any run of `secret` at least [`MIN_LEAK_WINDOW`] characters
/// long, naming the window and what leaked (`what`: "a rotated token", "a typed credential"). Every
/// window, not just the whole string: the failure this guards against is a partial write.
pub fn assert_no_secret_window(raw: &str, secret: &str, what: &str) {
    let chars: Vec<char> = secret.chars().collect();
    assert!(chars.len() >= MIN_LEAK_WINDOW, "the fixture is shorter than the window");
    for start in 0..=chars.len() - MIN_LEAK_WINDOW {
        for end in (start + MIN_LEAK_WINDOW)..=chars.len() {
            let window: String = chars[start..end].iter().collect();
            assert!(
                !raw.contains(&window),
                "the change journal carries a {}-character window of {what} ({window:?}). The                  record type takes no value parameter, so this means a value reached a cell that                  was supposed to hold a NAME.
ledger: {raw}",
                end - start
            );
        }
    }
}
