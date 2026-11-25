//! `word_enum!`'s own contract, on one enum of each form. The vocabularies other crates declare
//! through it are pinned word for word by their owners (`crates/vike-model/src/word_enum_tests.rs`).

crate::word_enum! {
    /// A DEFINING-form vocabulary: the invocation declares the enum.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Defined {
        /// A one-word member.
        First => "first",
        SecondWord => "second-word",
    }
}

/// An IMPL-ONLY-form vocabulary: the enum is a literal block, the invocation adds the members.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImplOnly {
    Alpha,
    Beta,
}

crate::word_enum! {
    impl ImplOnly {
        Alpha => "alpha",
        Beta => "Beta",
    }
}

#[test]
fn the_defining_form_declares_the_enum_and_every_member() {
    assert_eq!(Defined::ALL, [Defined::First, Defined::SecondWord]);
    assert_eq!(Defined::WORDS, ["first", "second-word"]);
    assert_eq!(Defined::SecondWord.word(), "second-word");
    assert_eq!(Defined::from_word("second-word"), Some(Defined::SecondWord));
    assert_eq!(Defined::from_word("SECOND-WORD"), None, "from_word is exact");
    assert_eq!(
        Defined::parse("SECOND-word"),
        Some(Defined::SecondWord),
        "parse ignores ASCII case"
    );
    assert_eq!(Defined::parse(" first"), None, "nothing here trims");
    assert_eq!(Defined::parse(""), None);
}

#[test]
fn the_impl_only_form_adds_the_members_to_a_literal_enum() {
    assert_eq!(ImplOnly::ALL, [ImplOnly::Alpha, ImplOnly::Beta]);
    assert_eq!(ImplOnly::WORDS, ["alpha", "Beta"]);
    for member in ImplOnly::ALL {
        assert_eq!(ImplOnly::from_word(member.word()), Some(member));
    }
    assert_eq!(ImplOnly::from_word("beta"), None, "from_word is exact");
    assert_eq!(ImplOnly::parse("BETA"), Some(ImplOnly::Beta), "parse ignores ASCII case");
    assert_eq!(ImplOnly::parse("gamma"), None);
}
