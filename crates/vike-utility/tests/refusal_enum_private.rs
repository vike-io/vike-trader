//! A refusal enum and a word vocabulary declared in a crate-PRIVATE position (this test crate stands in
//! for a binary such as `vike-cli`, which exports nothing): the caller uses only what it needs (the
//! refusal's `Display`, one word), and `cargo clippy --all-targets -- -D warnings` must stay green over
//! every generated member it never names.

vike_utility::refusal_enum! {
    /// A private vocabulary only ever displayed.
    #[derive(Debug)]
    enum PrivateRefusal with ids PrivateRefusalId {
        Empty => "private.empty": "the profile is empty",
        TooShort { step: u32 } => "private.too_short": "step {step} is too short",
    }
}

vike_utility::word_enum! {
    /// A private vocabulary whose only member in use is `word`.
    #[derive(Debug, Clone, Copy)]
    enum PrivateWord {
        Mainnet => "mainnet",
    }
}

#[test]
fn a_private_refusal_is_displayed_and_a_private_word_read() {
    let texts =
        [PrivateRefusal::Empty, PrivateRefusal::TooShort { step: 2 }].map(|r| r.to_string());
    assert_eq!(texts, ["the profile is empty", "step 2 is too short"]);
    assert_eq!(PrivateWord::Mainnet.word(), "mainnet");
}
