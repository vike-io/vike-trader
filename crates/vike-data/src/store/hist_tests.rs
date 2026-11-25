use super::*;

/// The two shapes the process-boundary copies were declared with until B5.11 deleted them, field for field
/// and variant for variant (the backtesthub sandbox's wire range and error, the user-research file's range
/// and fault): their bytes are what the shared derive must keep writing. A derive writes no type name into a JSON or a named msgpack body, so equal
/// JSON from the same derive is equal msgpack too (the two codecs those copies ride).
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct CopiedRange {
    start: Option<i64>,
    end: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
enum CopiedError {
    Query(String),
    Io(String),
    Unsupported(String),
}

/// **The serde forms are the wire copies' bytes**, both ways: a range and every error variant write what
/// the copy writes, the copy's bytes read back as the same value, and a body that names no bound reads
/// as the unbounded range through either.
#[test]
fn the_serde_forms_are_the_wire_copies_bytes() {
    for range in [
        TimestampRange::of(1, 2),
        TimestampRange { start: None, end: Some(5) },
        TimestampRange::all(),
    ] {
        let copy = CopiedRange { start: range.start, end: range.end };
        let bytes = serde_json::to_string(&copy).unwrap();
        assert_eq!(serde_json::to_string(&range).unwrap(), bytes);
        assert_eq!(serde_json::from_str::<TimestampRange>(&bytes).unwrap(), range);
    }
    assert_eq!(serde_json::from_str::<TimestampRange>("{}").unwrap(), TimestampRange::all());
    assert_eq!(
        serde_json::from_str::<CopiedRange>("{}").unwrap(),
        CopiedRange { start: None, end: None }
    );

    for (error, copy) in [
        (HistoryStoreError::Query("q".into()), CopiedError::Query("q".into())),
        (HistoryStoreError::Io("i".into()), CopiedError::Io("i".into())),
        (HistoryStoreError::Unsupported("u".into()), CopiedError::Unsupported("u".into())),
    ] {
        let bytes = serde_json::to_string(&copy).unwrap();
        assert_eq!(serde_json::to_string(&error).unwrap(), bytes);
        let back: HistoryStoreError = serde_json::from_str(&bytes).unwrap();
        assert_eq!(back.to_string(), error.to_string(), "{bytes}");
    }
}
