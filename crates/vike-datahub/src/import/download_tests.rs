use super::*;

const DAY: i64 = MS_PER_DAY;
/// 2024-01-31.
const JAN31: i64 = 19_753 * DAY;

/// A window across a year end lists each month once, the month ZERO-based as the bucket spells it.
#[test]
fn each_month_of_the_window_is_one_prefix() {
    assert_eq!(month_prefixes("EURUSD", JAN31, JAN31), vec!["EURUSD/2024/00/"]);
    assert_eq!(
        month_prefixes("EURUSD", JAN31, JAN31 + DAY),
        vec!["EURUSD/2024/00/", "EURUSD/2024/01/"]
    );
    let dec31_2023 = JAN31 - 31 * DAY;
    assert_eq!(
        month_prefixes("XAUUSD", dec31_2023, JAN31),
        vec!["XAUUSD/2023/11/", "XAUUSD/2024/00/"]
    );
}

/// No key: the two `secrets set` lines, the value on stdin, and the IAM rights.
#[test]
fn the_missing_key_message_says_what_to_run() {
    let text = no_key_message();
    assert!(text.contains("vike-cli secrets set DUKASCOPY_ARCHIVE_AWS_ACCESS_KEY_ID"), "{text}");
    assert!(
        text.contains("vike-cli secrets set DUKASCOPY_ARCHIVE_AWS_SECRET_ACCESS_KEY"),
        "{text}"
    );
    assert!(text.contains("stdin") && text.contains("s3:GetObject"), "{text}");
}

struct NoKeys;

impl ToolFeedKeyReader for NoKeys {
    fn read(&self, _name: &str) -> ToolFeedKeyRead {
        ToolFeedKeyRead::Absent
    }
}

/// A 403 names the bucket and the two IAM permissions; a wrong pair says so instead.
#[test]
fn a_refusal_names_the_rights_the_key_lacks() {
    let download = DukascopyDownload::new(
        "https://cfg-public-proper-wallaby.s3.eu-west-1.amazonaws.com",
        "eu-west-1",
        Arc::new(NoKeys),
    );
    let denied = S3Error::Status { status: 403, code: Some("AccessDenied".to_string()) };
    let text = download.refusal("listing EURUSD/2024/00/", &denied);
    assert!(text.contains("s3:ListBucket on arn:aws:s3:::cfg-public-proper-wallaby"), "{text}");
    assert!(text.contains("s3:GetObject on arn:aws:s3:::cfg-public-proper-wallaby/*"), "{text}");
    let wrong = S3Error::Status { status: 403, code: Some("InvalidAccessKeyId".to_string()) };
    assert!(download.refusal("x", &wrong).contains("the stored key pair is wrong"));
    let none = download.key().err().expect("no key is a refusal");
    assert_eq!(none, no_key_message());
}
