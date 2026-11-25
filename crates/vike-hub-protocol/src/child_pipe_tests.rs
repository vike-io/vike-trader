//! The child pipe's codec and its build check.

use std::io::ErrorKind;

use serde::{Deserialize, Serialize};

use super::{check_build, named, read_message, send_build, write_message, write_message_named};

/// A message with a field left out at its default BEFORE its last field: the shape positional cannot carry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Gapped {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
    price: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last: Option<u32>,
}

/// The same payload as an island inside a positional message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Holder {
    sequence: u64,
    #[serde(with = "named")]
    inner: Gapped,
    after: Vec<f64>,
}

fn written(write: fn(&mut Vec<u8>, &Gapped) -> std::io::Result<()>, msg: &Gapped) -> Vec<u8> {
    let mut bytes = Vec::new();
    write(&mut bytes, msg).expect("a frame");
    bytes
}

/// A message crosses positional and named alike, NaN's bits included, and positional is the smaller; one whose
/// gap is not at the end crosses only named or in an island, and a trailing gap reads back as its default.
#[test]
fn a_message_crosses_positional_and_named_and_a_gap_needs_names() {
    let whole = Gapped {
        tag: Some("t".into()),
        price: f64::from_bits(0x7ff8_0000_0000_0001),
        last: Some(3),
    };
    let trailing = Gapped { last: None, ..whole.clone() };
    for msg in [&whole, &trailing] {
        let positional = written(write_message, msg);
        let names = written(write_message_named, msg);
        assert!(positional.len() < names.len(), "{} >= {}", positional.len(), names.len());
        for bytes in [positional, names] {
            let back: Gapped = read_message(&mut bytes.as_slice()).expect("reads");
            assert_eq!(back.price.to_bits(), msg.price.to_bits(), "the NaN keeps its bits");
            assert_eq!((back.tag.as_deref(), back.last), (msg.tag.as_deref(), msg.last));
        }
    }

    let gap = Gapped { tag: None, ..whole };
    let positional = written(write_message, &gap);
    assert!(
        read_message::<_, Gapped>(&mut positional.as_slice())
            .is_err_and(|e| e.kind() == ErrorKind::InvalidData),
        "a gap before the last field does not read back positional"
    );
    let names = written(write_message_named, &gap);
    let back: Gapped = read_message(&mut names.as_slice()).expect("named reads");
    assert_eq!((back.tag, back.last), (None, Some(3)));

    let holder = Holder { sequence: 9, inner: gap, after: vec![1.5, -0.0] };
    let mut bytes = Vec::new();
    write_message(&mut bytes, &holder).expect("a frame");
    let back: Holder = read_message(&mut bytes.as_slice()).expect("the island reads");
    assert_eq!(format!("{:?}", back.inner), format!("{:?}", holder.inner), "NaN is no PartialEq");
    assert_eq!((back.sequence, back.after[1].to_bits()), (9, (-0.0f64).to_bits()));
}

/// The same build passes; another build, and a first frame that is no build, are refused in words naming both;
/// a pipe that ends first is an end of stream, not a refusal.
#[test]
fn another_build_is_refused_in_words() {
    let ours =
        "git 1111aaaa clean, built 2026-11-19T00:00:00Z, rustc 1.96.0, x86_64-unknown-linux-gnu";
    let theirs =
        "git 2222bbbb clean, built 2026-11-18T00:00:00Z, rustc 1.96.0, x86_64-unknown-linux-gnu";
    let build = |b: &str| {
        let mut bytes = Vec::new();
        send_build(&mut bytes, b).expect("a frame");
        bytes
    };
    check_build(&mut build(ours).as_slice(), ours, "the child").expect("one build");

    let refused =
        check_build(&mut build(theirs).as_slice(), ours, "the child").expect_err("another build");
    assert_eq!(refused.kind(), ErrorKind::Unsupported);
    let words = refused.to_string();
    assert!(
        words.contains("the child") && words.contains(ours) && words.contains(theirs),
        "{words}"
    );

    let mut older = Vec::new();
    write_message_named(&mut older, &Gapped { tag: None, price: 1.0, last: None })
        .expect("a frame");
    let refused = check_build(&mut older.as_slice(), ours, "the hub").expect_err("no build");
    assert_eq!(refused.kind(), ErrorKind::Unsupported);
    assert!(
        refused.to_string().contains("no build") && refused.to_string().contains(ours),
        "{refused}"
    );

    let mut nothing: &[u8] = &[];
    let ended = check_build(&mut nothing, ours, "the hub").expect_err("nothing came");
    assert_eq!(ended.kind(), ErrorKind::UnexpectedEof);
}
