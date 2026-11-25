//! `refusal_enum!` on a sample vocabulary: the text renders the fields, the template is the literal,
//! the ids are the declared words (unique, read back exactly) and `ALL` is every variant; the one
//! renderer's form; and the carrier: `carried()`, free text's `.into()`, the `carries` marker and the
//! one-id rule at any depth.

use crate::errors::{CarriedRefusal, Declared, render_refusal, render_refusal_or_text};

crate::refusal_enum! {
    /// A sample vocabulary: fields in another order than the text names them, a `Debug` hole, an
    /// escaped brace and a unit variant.
    #[derive(Debug, Clone, PartialEq)]
    pub enum SampleRefusal with ids SampleRefusalId {
        /// Two holes, named in the text's order, not the fields'.
        StepShorterThanTest { test: u32, step: u32 }
            => "sample.walkforward.step_shorter_than_test":
               "walkforward step {step} is shorter than its test window {test}",
        UnknownWord { word: String }
            => "sample.unknown_word": "unknown word {word:?} (want {{a | b}})",
        Empty => "sample.empty": "the profile is empty",
    }
}

/// One refusal of every variant, in declaration order.
fn one_of_each() -> [SampleRefusal; 3] {
    [
        SampleRefusal::StepShorterThanTest { test: 5, step: 2 },
        SampleRefusal::UnknownWord { word: "c".to_string() },
        SampleRefusal::Empty,
    ]
}

#[test]
fn display_renders_the_fields_into_the_holes() {
    let texts = one_of_each().map(|refusal| refusal.to_string());
    assert_eq!(
        texts,
        [
            "walkforward step 2 is shorter than its test window 5",
            "unknown word \"c\" (want {a | b})",
            "the profile is empty",
        ]
    );
}

#[test]
fn the_template_is_the_literal_itself() {
    let templates = one_of_each().map(|refusal| refusal.template());
    assert_eq!(
        templates,
        [
            "walkforward step {step} is shorter than its test window {test}",
            "unknown word {word:?} (want {{a | b}})",
            "the profile is empty",
        ]
    );
    for refusal in one_of_each() {
        assert_eq!(refusal.template(), refusal.id().template(), "{refusal:?}");
    }
}

/// The ids are the words the declaration sets, byte for byte, one per variant in declaration order,
/// none twice; `ALL` is complete because every variant's id is in it at its own place.
#[test]
fn the_ids_are_the_declared_words_unique_and_all_is_complete() {
    assert_eq!(
        SampleRefusalId::WORDS,
        ["sample.walkforward.step_shorter_than_test", "sample.unknown_word", "sample.empty"]
    );
    assert_eq!(one_of_each().map(|refusal| refusal.id()), SampleRefusalId::ALL);
    let mut words = SampleRefusalId::WORDS.to_vec();
    words.sort_unstable();
    words.dedup();
    assert_eq!(words.len(), SampleRefusalId::ALL.len(), "an id is listed twice");
    for id in SampleRefusalId::ALL {
        assert_eq!(SampleRefusalId::from_word(id.word()), Some(id));
    }
    assert_eq!(SampleRefusalId::from_word("sample.no_such_refusal"), None);
    assert_eq!(SampleRefusalId::parse("SAMPLE.EMPTY"), Some(SampleRefusalId::Empty));
}

#[test]
fn a_refusal_renders_for_a_person_as_its_id_then_its_text() {
    let refusal = SampleRefusal::StepShorterThanTest { test: 5, step: 2 };
    assert_eq!(
        render_refusal(refusal.id().word(), &refusal),
        "error [sample.walkforward.step_shorter_than_test]: walkforward step 2 is shorter than its test window 5"
    );
    assert_eq!(refusal.rendered(), render_refusal(refusal.id().word(), &refusal));
    assert_eq!(refusal.to_string(), "walkforward step 2 is shorter than its test window 5");
    assert_eq!(
        render_refusal("hub.access_denied", "access denied"),
        "error [hub.access_denied]: access denied"
    );
    // An optional id: rendered when present, the bare text when not.
    assert_eq!(
        render_refusal_or_text(Some("order.venue.rejected"), "too small"),
        "error [order.venue.rejected]: too small"
    );
    assert_eq!(render_refusal_or_text(None, "too small"), "too small");
}

crate::refusal_enum! {
    /// A private vocabulary only ever displayed: the members it never names stay quiet under
    /// `clippy -D warnings` in this crate's own expansion too, where rustc does not drop the lint
    /// (`crates/vike-utility/tests/refusal_enum_private.rs` is another crate's expansion).
    #[derive(Debug)]
    enum DisplayedOnly with ids DisplayedOnlyId {
        Empty => "displayed.empty": "nothing to show",
        Forwarded { why: CarriedRefusal } => carries why, "displayed.forwarded": "{why}",
    }
}

#[test]
fn a_private_refusal_used_only_through_display_renders() {
    assert_eq!(DisplayedOnly::Empty.to_string(), "nothing to show");
    assert_eq!(DisplayedOnly::Forwarded { why: "no pair".into() }.to_string(), "no pair");
}

crate::refusal_enum! {
    /// Carriers over [`SampleRefusal`] and over a carried value, and one variant left unmarked.
    #[derive(Debug, Clone, PartialEq)]
    pub enum CarrierRefusal with ids CarrierRefusalId {
        /// Forwards a refusal enum held in place.
        Sample { step: u32, inner: SampleRefusal } => carries inner,
            "carrier.sample": "step {step}: {inner}",
        /// Forwards a refusal that crossed a seam: an id, or free text.
        Carried { why: CarriedRefusal } => carries why, "carrier.carried": "cannot admit: {why}",
        /// Holds another sentence UNMARKED: its own id is the news.
        Detail { why: CarriedRefusal } => "carrier.detail": "held: {why}",
    }
}

crate::refusal_enum! {
    /// A carrier of a carrier: the depth case.
    #[derive(Debug, Clone, PartialEq)]
    pub enum OuterRefusal with ids OuterRefusalId {
        Forwarded { inner: CarrierRefusal } => carries inner, "outer.forwarded": "outer: {inner}",
    }
}

/// The one-id rule: `shown` holds exactly one `error [`, and it names `id`.
fn assert_one_id(shown: &str, id: &str) {
    assert_eq!(shown.matches("error [").count(), 1, "ONE id: {shown}");
    assert!(shown.starts_with(&format!("error [{id}]: ")), "{id}: {shown}");
}

#[test]
fn carried_keeps_the_id_and_the_bare_text() {
    let refusal = SampleRefusal::StepShorterThanTest { test: 5, step: 2 };
    let carried = refusal.carried();
    assert_eq!(carried.declared_id(), Some("sample.walkforward.step_shorter_than_test"));
    assert_eq!(carried.to_string(), refusal.to_string());
    assert_eq!(carried.rendered(), refusal.rendered());
    assert_eq!(refusal.declared_id(), Some(refusal.shown_id()));
    assert_eq!(refusal.shown_id(), refusal.id().word());
}

#[test]
fn free_text_converts_with_no_id_and_renders_bare() {
    let from_str = CarriedRefusal::from("the venue said no");
    let from_string: CarriedRefusal = "the venue said no".to_string().into();
    assert_eq!(from_str, from_string);
    assert_eq!(from_str, CarriedRefusal::new(None, "the venue said no".to_string()));
    assert_eq!(from_str.declared_id(), None);
    assert_eq!(from_str.to_string(), "the venue said no");
    assert_eq!(from_str.rendered(), "the venue said no");
    // `?` on a `Result<_, String>` converts by itself.
    fn answered(read: Result<u32, String>) -> Result<u32, CarriedRefusal> {
        Ok(read? + 1)
    }
    assert_eq!(answered(Err("no pair".to_string())), Err(CarriedRefusal::from("no pair")));
}

#[test]
fn a_carrier_shows_the_carried_id_else_its_own() {
    let inner = SampleRefusal::StepShorterThanTest { test: 5, step: 2 };
    let held = CarrierRefusal::Sample { step: 3, inner: inner.clone() };
    assert_eq!(held.id(), CarrierRefusalId::Sample, "`id()` stays the variant's own");
    assert_eq!(held.shown_id(), "sample.walkforward.step_shorter_than_test");
    assert_eq!(
        held.rendered(),
        "error [sample.walkforward.step_shorter_than_test]: step 3: walkforward step 2 is shorter \
         than its test window 5"
    );
    assert_eq!(held.carried().declared_id(), Some("sample.walkforward.step_shorter_than_test"));
    let crossed = CarrierRefusal::Carried { why: inner.carried() };
    assert_eq!(crossed.shown_id(), "sample.walkforward.step_shorter_than_test");
    // A carried value with no id shows its carrier's id; nothing invents one.
    let free = CarrierRefusal::Carried { why: "no such symbol".into() };
    assert_eq!(free.shown_id(), "carrier.carried");
    assert_eq!(free.rendered(), "error [carrier.carried]: cannot admit: no such symbol");
    // The marker is opt-in: an unmarked variant shows its own id whatever it holds.
    let detail = CarrierRefusal::Detail { why: inner.carried() };
    assert_eq!(detail.shown_id(), "carrier.detail");
}

/// However many seams and carriers a refusal crosses, the person reads its innermost id, once.
#[test]
fn a_refusal_carried_then_wrapped_shows_exactly_one_id() {
    let inner = SampleRefusal::UnknownWord { word: "c".to_string() };
    assert_one_id(
        &CarrierRefusal::Sample { step: 1, inner: inner.clone() }.rendered(),
        "sample.unknown_word",
    );
    assert_one_id(
        &CarrierRefusal::Carried { why: inner.carried() }.rendered(),
        "sample.unknown_word",
    );
    let twice = OuterRefusal::Forwarded {
        inner: CarrierRefusal::Carried { why: CarrierRefusal::Sample { step: 1, inner }.carried() },
    };
    assert_one_id(&twice.rendered(), "sample.unknown_word");
    assert_one_id(&twice.carried().rendered(), "sample.unknown_word");
    assert_eq!(twice.to_string(), "outer: cannot admit: step 1: unknown word \"c\" (want {a | b})");
    let free = OuterRefusal::Forwarded { inner: CarrierRefusal::Carried { why: "x".into() } };
    assert_one_id(&free.rendered(), "carrier.carried");
}

/// A rendered text handed into a carrier would show its id twice once the carrier is rendered: every
/// debug build refuses it at the door.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "a carried refusal holds the BARE text")]
fn a_rendered_text_cannot_enter_a_carrier() {
    let _ = CarriedRefusal::from(SampleRefusal::Empty.rendered());
}
