//! The signature against the AWS signing test suite (`aws-c-auth`'s `tests/aws-signing-test-suite/v4`,
//! read 2026-11-13: `context.json` gives the key, the region `us-east-1`, the service `service` and the
//! time `20150830T123600Z`; each case's `header-canonical-request.txt` and `header-signature.txt` are
//! what is compared), the listing's XML, and the base's region.

use super::*;

fn suite_key() -> S3Key {
    S3Key {
        access_key_id: Secret::new("AKIDEXAMPLE".to_string()),
        secret_access_key: Secret::new("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string()),
    }
}

const SUITE_DATE: &str = "20150830T123600Z";

/// The suite's header-signed request: GET, `us-east-1`, `service`, an empty body.
fn suite(path: &str, query: &[(&str, &str)], extra: &[(&str, &str)]) -> String {
    let mut headers = vec![("host", "example.amazonaws.com"), ("x-amz-date", SUITE_DATE)];
    headers.extend_from_slice(extra);
    authorization(
        &suite_key(),
        SUITE_DATE,
        "us-east-1",
        "service",
        "GET",
        path,
        query,
        &headers,
        EMPTY_PAYLOAD_SHA256,
    )
}

fn signature_of(authorization: &str) -> &str {
    authorization.rsplit("Signature=").next().expect("a signature")
}

#[test]
fn get_vanilla() {
    let auth = suite("/", &[], &[]);
    assert_eq!(
        auth,
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
         SignedHeaders=host;x-amz-date, \
         Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
    );
}

/// The query is sorted by name, whatever order the request names it in.
#[test]
fn get_vanilla_query_order_key_case() {
    let auth = suite("/", &[("Param2", "value2"), ("Param1", "value1")], &[]);
    assert_eq!(
        signature_of(&auth),
        "b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500"
    );
}

/// The unreserved characters pass through the query unencoded.
#[test]
fn get_vanilla_query_unreserved() {
    let unreserved = "-._~0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let auth = suite("/", &[(unreserved, unreserved)], &[]);
    assert_eq!(
        signature_of(&auth),
        "9c3e54bfcdf0b19771a7f523ee5669cdf59bc7cc0884027167c21bb143a40197"
    );
}

/// A header's value is trimmed and its runs of spaces collapsed; its name is lower-cased and every
/// signed header is listed, sorted.
#[test]
fn get_header_value_trim() {
    let auth = suite("/", &[], &[("My-Header1", " value1"), ("My-Header2", "\"a   b   c\"")]);
    assert!(auth.contains("SignedHeaders=host;my-header1;my-header2;x-amz-date,"), "{auth}");
    assert_eq!(
        signature_of(&auth),
        "acc3ed3afb60bb290fc8d2dd0098b9911fcaa05412b367055dee359757a9c736"
    );
}

/// A key's path keeps its `/` and encodes each segment once; a listing prefix's `/` is encoded in
/// the query.
#[test]
fn a_key_path_and_a_prefix_are_encoded_the_way_s3_reads_them() {
    assert_eq!(canonical_path("EURUSD/2024/00/02_ticks.bi5"), "/EURUSD/2024/00/02_ticks.bi5");
    assert_eq!(canonical_path("A B/c+d"), "/A%20B/c%2Bd");
    assert_eq!(
        canonical_query(&[("prefix", "EURUSD/2024/00/"), ("list-type", "2")]),
        "list-type=2&prefix=EURUSD%2F2024%2F00%2F"
    );
}

/// The key reaches the header only as its id; the secret never does.
#[test]
fn the_header_carries_the_key_id_and_never_the_secret() {
    let auth = suite("/", &[], &[]);
    assert!(auth.contains("AKIDEXAMPLE"));
    assert!(!auth.contains("wJalrXUtnFEMI"));
}

#[test]
fn a_listing_page_is_read_with_its_continuation() {
    let page = br#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>b</Name><Prefix>EURUSD/2024/00/</Prefix>
<KeyCount>2</KeyCount><MaxKeys>2</MaxKeys><IsTruncated>true</IsTruncated>
<Contents><Key>EURUSD/2024/00/02_ticks.bi5</Key><LastModified>2024-01-03T00:00:00.000Z</LastModified><Size>312345</Size></Contents>
<Contents><Key>EURUSD/2024/00/03_ticks.bi5</Key><Size>0</Size></Contents>
<NextContinuationToken>1a+b/c=</NextContinuationToken></ListBucketResult>"#;
    let read = parse_list_page(page).expect("a page");
    assert_eq!(
        read.objects,
        vec![
            S3Object { key: "EURUSD/2024/00/02_ticks.bi5".to_string(), bytes: 312_345 },
            S3Object { key: "EURUSD/2024/00/03_ticks.bi5".to_string(), bytes: 0 },
        ]
    );
    assert_eq!(read.next.as_deref(), Some("1a+b/c="));
    let last = br#"<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>"#;
    assert_eq!(parse_list_page(last).expect("a page"), ListPage { objects: vec![], next: None });
}

/// An answer that is not a listing is an error, never an empty listing; a truncated page without a
/// token is an error, never the end.
#[test]
fn a_page_that_is_not_a_listing_is_refused() {
    assert!(parse_list_page(b"<Error><Code>AccessDenied</Code></Error>").is_err());
    assert!(
        parse_list_page(b"<ListBucketResult><IsTruncated>true</IsTruncated></ListBucketResult>")
            .is_err()
    );
    assert_eq!(
        tag_text("<Error><Code>AccessDenied</Code></Error>", "Code").as_deref(),
        Some("AccessDenied")
    );
}

#[test]
fn the_base_names_its_region_and_bucket() {
    assert_eq!(
        region_and_bucket("https://cfg-public-proper-wallaby.s3.eu-west-1.amazonaws.com"),
        Some(("eu-west-1".to_string(), "cfg-public-proper-wallaby".to_string()))
    );
    assert_eq!(region_and_bucket("https://s3.amazonaws.com/bucket"), None);
    assert_eq!(region_and_bucket("http://b.s3.eu-west-1.amazonaws.com"), None);
}

/// Over the ONE shared ladder (`vike_utility::retry::retry_on_pauses`) with this crate's [`settled`]
/// rule: only a transport failure, a 429 and a 5xx are asked again, on [`RETRY_PAUSES`]; a 403 or a
/// 404 is answered at once, and with the pauses spent the last failure stands.
#[test]
fn only_a_transient_failure_is_asked_again() {
    let slept = std::cell::Cell::new(0);
    let sleep = |_: Duration| slept.set(slept.get() + 1);
    let ladder = |answers: &mut dyn FnMut(u32) -> Result<u8, S3Error>| {
        vike_utility::retry::retry_on_pauses(&RETRY_PAUSES, answers, settled, sleep)
    };
    for status in [403, 404] {
        let mut asks = 0;
        let refused = ladder(&mut |_| {
            asks += 1;
            Err(S3Error::Status { status, code: None })
        });
        assert!(refused.is_err());
        assert_eq!((asks, slept.get()), (1, 0), "HTTP {status} is answered at once");
    }
    for status in [429, 503] {
        slept.set(0);
        let mut asks = 0;
        let busy = ladder(&mut |_| {
            asks += 1;
            if asks < 3 { Err(S3Error::Status { status, code: None }) } else { Ok(7) }
        });
        assert_eq!(busy, Ok(7));
        assert_eq!((asks, slept.get()), (3, 2), "HTTP {status} is asked again");
    }
    slept.set(0);
    let mut asks = 0;
    let down = ladder(&mut |_| {
        asks += 1;
        Err(S3Error::Transport("timed out".to_string()))
    });
    assert!(down.is_err());
    assert_eq!((asks, slept.get()), (RETRY_PAUSES.len() + 1, RETRY_PAUSES.len()));
}
