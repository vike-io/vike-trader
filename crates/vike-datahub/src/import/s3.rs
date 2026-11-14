//! **An S3 bucket read with the operator's own AWS key**: the AWS Signature Version 4 header, and
//! the two requests the archive download sends, `ListObjectsV2` and `GetObject`, each with
//! `x-amz-request-payer: requester` (a Requester-Pays bucket bills the key's account, and refuses a
//! request that does not say it will pay).
//!
//! # The shared stack only
//!
//! `ureq` (rustls with ring) for the transport, and `hmac`/`sha2` for the signature: the packages the
//! bridge signers use. No AWS SDK (`deny.toml` bans a second stack), and not the bridge signers
//! themselves: this crate links no `vike-bridge-core` in any feature set (`DATAHUB_FENCE`). The
//! signature is pinned by the AWS signing test suite's vectors (`s3_tests.rs`).
//!
//! # ⚠ The key reaches no log line, error text or `Debug`
//!
//! It is held as two [`Secret`]s, it is used only to build the `Authorization` header, and no error
//! here formats a request, a header or a `ureq` error: [`S3Error`] carries a status, the bucket's
//! own error CODE (`AccessDenied`) and fixed words.

use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use vike_model::secret::Secret;
use vike_utility::hex::hex_lower;
use vike_utility::percent::percent_encode;

type HmacSha256 = Hmac<Sha256>;

/// The SHA-256 of an empty payload, in hex: every request here is a GET without a body.
const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// The signing service of every request here.
const SERVICE: &str = "s3";

/// One request's whole wait, a listing page or a daily file of a few megabytes.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// How long opening the socket may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// The largest listing page read: a thousand keys of about two hundred bytes of XML each.
const LIST_PAGE_CAP: u64 = 4 * 1024 * 1024;

/// The pauses between the attempts of one request that failed TRANSIENTLY ([`S3Error::transient`]):
/// four retries, about half a minute in all, the burst a busy S3 prefix answers `503 SlowDown` for.
pub const RETRY_PAUSES: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(8),
    Duration::from_secs(20),
];

/// The operator's AWS key pair, as the datahub's binary read it from its own settings database.
pub struct S3Key {
    /// The access key id (`AKIA…`): not secret on its own, held as one all the same.
    pub access_key_id: Secret<String>,
    /// The secret access key.
    pub secret_access_key: Secret<String>,
}

/// Why one request failed. No variant carries a URL, a header or a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S3Error {
    /// The bucket answered with this status and, when its body named one, its error code
    /// (`AccessDenied`, `NoSuchKey`, `SlowDown`).
    Status { status: u16, code: Option<String> },
    /// No usable answer: DNS, connect, TLS, a reset, a timeout, a body cut off.
    Transport(String),
    /// A body this side could not read (a listing that is not ListObjectsV2's XML, an object over
    /// its cap).
    Body(String),
}

impl S3Error {
    /// Whether asking again may help: a transport failure, a 429 or a 5xx.
    pub fn transient(&self) -> bool {
        match self {
            S3Error::Transport(_) => true,
            S3Error::Status { status, .. } => *status == 429 || *status >= 500,
            S3Error::Body(_) => false,
        }
    }
}

impl std::fmt::Display for S3Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            S3Error::Status { status, code: Some(code) } => write!(f, "HTTP {status} ({code})"),
            S3Error::Status { status, code: None } => write!(f, "HTTP {status}"),
            S3Error::Transport(why) | S3Error::Body(why) => f.write_str(why),
        }
    }
}

/// One key of a listing: its name and its size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Object {
    pub key: String,
    pub bytes: u64,
}

/// **One bucket, addressed by its base** (`https://<bucket>.s3.<region>.amazonaws.com`, a
/// virtual-hosted base, or a loopback stand-in in a test), signed for `region` with `key`.
pub struct S3Bucket {
    base: String,
    host: String,
    region: String,
    key: S3Key,
    agent: ureq::Agent,
}

impl S3Bucket {
    /// A bucket at `base` (scheme and authority, no path) in `region`. `Err` when `base` is not
    /// `http(s)://authority`.
    pub fn new(base: &str, region: &str, key: S3Key) -> Result<S3Bucket, String> {
        let authority = base
            .strip_prefix("https://")
            .or_else(|| base.strip_prefix("http://"))
            .filter(|a| !a.is_empty() && !a.contains('/'))
            .ok_or_else(|| {
                format!("an S3 base is `https://<bucket>.s3.<region>.amazonaws.com`, not {base:?}")
            })?;
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(REQUEST_TIMEOUT))
            // A status is read, not raised: a 403's body names WHY.
            .http_status_as_error(false)
            // A signed request is never carried to another host.
            .max_redirects(0)
            .build()
            .into();
        Ok(S3Bucket {
            base: base.to_string(),
            host: authority.to_string(),
            region: region.to_string(),
            key,
            agent,
        })
    }

    /// Every key under `prefix`, following `ListObjectsV2`'s continuation, and how many LIST
    /// requests that took (each billed), retries included.
    pub fn list(
        &self,
        prefix: &str,
        sleep: &dyn Fn(Duration),
    ) -> (Result<Vec<S3Object>, S3Error>, u64) {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        let mut requests = 0;
        loop {
            let mut query = vec![("list-type", "2"), ("prefix", prefix)];
            if let Some(t) = &token {
                query.push(("continuation-token", t.as_str()));
            }
            let page = vike_utility::retry::retry_on_pauses(
                &RETRY_PAUSES,
                |_| {
                    requests += 1;
                    self.get("/", &query, LIST_PAGE_CAP)
                },
                settled,
                sleep,
            );
            let page = match page.and_then(|body| parse_list_page(&body)) {
                Ok(page) => page,
                Err(e) => return (Err(e), requests),
            };
            out.extend(page.objects);
            match page.next {
                Some(next) => token = Some(next),
                None => return (Ok(out), requests),
            }
        }
    }

    /// The object at `key`, refused when it is over `cap` bytes, and how many GET requests that
    /// took (each billed), retries included.
    pub fn object(
        &self,
        key: &str,
        cap: u64,
        sleep: &dyn Fn(Duration),
    ) -> (Result<Vec<u8>, S3Error>, u64) {
        let mut requests = 0;
        let path = canonical_path(key);
        let got = vike_utility::retry::retry_on_pauses(
            &RETRY_PAUSES,
            |_| {
                requests += 1;
                self.get(&path, &[], cap)
            },
            settled,
            sleep,
        );
        (got, requests)
    }

    /// One signed GET of `path` (already URI-encoded) with `query`, its 2xx body read up to `cap`.
    fn get(&self, path: &str, query: &[(&str, &str)], cap: u64) -> Result<Vec<u8>, S3Error> {
        let amz_date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let headers = [
            ("host", self.host.as_str()),
            ("x-amz-content-sha256", EMPTY_PAYLOAD_SHA256),
            ("x-amz-date", amz_date.as_str()),
            ("x-amz-request-payer", "requester"),
        ];
        let authorization = authorization(
            &self.key,
            &amz_date,
            &self.region,
            SERVICE,
            "GET",
            path,
            query,
            &headers,
            EMPTY_PAYLOAD_SHA256,
        );
        let query_text = canonical_query(query);
        let url = if query_text.is_empty() {
            format!("{}{path}", self.base)
        } else {
            format!("{}{path}?{query_text}", self.base)
        };
        let mut call = self.agent.get(&url);
        // `host` rides the URL: ureq writes it from the authority, which is the signed value.
        for (name, value) in headers.iter().filter(|(name, _)| *name != "host") {
            call = call.header(*name, *value);
        }
        let mut response = call
            .header("authorization", authorization.as_str())
            .call()
            .map_err(|e| transport(&e))?;
        let status = response.status().as_u16();
        let body = response.body_mut().with_config().limit(cap).read_to_vec();
        if (200..300).contains(&status) {
            return body.map_err(|e| match e {
                ureq::Error::BodyExceedsLimit(_) => S3Error::Body(format!(
                    "the object is larger than this side's cap of {cap} bytes"
                )),
                other => transport(&other),
            });
        }
        let code = body.ok().and_then(|b| tag_text(&String::from_utf8_lossy(&b), "Code"));
        Err(S3Error::Status { status, code })
    }
}

/// A transport failure in a fixed vocabulary: a `ureq` error is never formatted, since its text is
/// not promised to omit the request (`crate::tool_feeds::http`'s `describe` rule).
fn transport(error: &ureq::Error) -> S3Error {
    S3Error::Transport(match error {
        ureq::Error::Timeout(_) => "timed out".to_string(),
        ureq::Error::Io(io) => format!("I/O error ({:?})", io.kind()),
        ureq::Error::HostNotFound => "host not found".to_string(),
        ureq::Error::ConnectionFailed => "connection failed".to_string(),
        _ => "request failed".to_string(),
    })
}

/// The "settled" rule this crate hands the one shared ladder (`vike_utility::retry::retry_on_pauses`,
/// over [`RETRY_PAUSES`]): every answer but a TRANSIENT failure ([`S3Error::transient`]: a transport
/// failure, a 429 or a 5xx) settles a request, so a 403 or a 404 is answered at once and only those
/// three are asked again.
fn settled<T>(answer: &Result<T, S3Error>) -> bool {
    !matches!(answer, Err(e) if e.transient())
}

// ─── AWS Signature Version 4 ─────────────────────────────────────────────────────────────────

/// `/` and each segment of `key` URI-encoded (S3 encodes a path once and keeps its `/`).
pub fn canonical_path(key: &str) -> String {
    let segments: Vec<String> = key.split('/').map(percent_encode).collect();
    format!("/{}", segments.join("/"))
}

/// The canonical query string: each name and value URI-encoded, sorted by name then value.
fn canonical_query(query: &[(&str, &str)]) -> String {
    let mut pairs: Vec<(String, String)> =
        query.iter().map(|(n, v)| (percent_encode(n), percent_encode(v))).collect();
    pairs.sort();
    pairs.iter().map(|(n, v)| format!("{n}={v}")).collect::<Vec<_>>().join("&")
}

fn hmac(key: &[u8], message: &str) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(message.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// **The `Authorization` header of one request**, AWS Signature Version 4. `amz_date` is the
/// request's `x-amz-date` (`YYYYMMDDTHHMMSSZ`); `path` is already URI-encoded; `headers` are the
/// SIGNED headers, lower-case names, `host` among them; `payload_sha256` is the hex digest of the
/// body.
pub fn authorization(
    key: &S3Key,
    amz_date: &str,
    region: &str,
    service: &str,
    method: &str,
    path: &str,
    query: &[(&str, &str)],
    headers: &[(&str, &str)],
    payload_sha256: &str,
) -> String {
    let mut sorted: Vec<(String, String)> = headers
        .iter()
        .map(|(n, v)| (n.to_ascii_lowercase(), v.split_whitespace().collect::<Vec<_>>().join(" ")))
        .collect();
    sorted.sort();
    let canonical_headers: String = sorted.iter().map(|(n, v)| format!("{n}:{v}\n")).collect();
    let signed_headers = sorted.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(";");
    let canonical_request = format!(
        "{method}\n{path}\n{}\n{canonical_headers}\n{signed_headers}\n{payload_sha256}",
        canonical_query(query)
    );
    let date = &amz_date[..amz_date.len().min(8)];
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex_lower(&Sha256::digest(canonical_request.as_bytes()))
    );
    let secret = format!("AWS4{}", key.secret_access_key.expose());
    let signing = [date, region, service, "aws4_request"]
        .iter()
        .fold(secret.into_bytes(), |k, part| hmac(&k, part));
    let signature = hex_lower(&hmac(&signing, &string_to_sign));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, \
         Signature={signature}",
        key.access_key_id.expose()
    )
}

// ─── ListObjectsV2's XML ──────────────────────────────────────────────────────────────────────

/// One listing page: its objects and the continuation token when it is truncated.
#[derive(Debug, PartialEq, Eq)]
struct ListPage {
    objects: Vec<S3Object>,
    next: Option<String>,
}

/// The text of the first `<tag>…</tag>` in `xml`, its five XML entities decoded.
fn tag_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = start + xml[start..].find(&close)?;
    Some(
        xml[start..end]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

/// Read one `ListObjectsV2` page: every `<Contents>`'s `<Key>` and `<Size>`, and
/// `<NextContinuationToken>` when `<IsTruncated>` is `true`. A page that is not that XML is an error,
/// never an empty listing.
fn parse_list_page(body: &[u8]) -> Result<ListPage, S3Error> {
    let xml = std::str::from_utf8(body)
        .map_err(|_| S3Error::Body("the bucket's listing is not UTF-8".to_string()))?;
    if !xml.contains("<ListBucketResult") {
        return Err(S3Error::Body(
            "the bucket's answer is not a ListObjectsV2 listing".to_string(),
        ));
    }
    let mut objects = Vec::new();
    for chunk in xml.split("<Contents>").skip(1) {
        let item = chunk.split("</Contents>").next().unwrap_or_default();
        let key = tag_text(item, "Key");
        let bytes = tag_text(item, "Size").and_then(|s| s.trim().parse::<u64>().ok());
        let (Some(key), Some(bytes)) = (key, bytes) else {
            return Err(S3Error::Body("a listed object carries no Key or Size".to_string()));
        };
        objects.push(S3Object { key, bytes });
    }
    let truncated = tag_text(xml, "IsTruncated").is_some_and(|t| t.trim() == "true");
    let next = if truncated {
        Some(tag_text(xml, "NextContinuationToken").ok_or_else(|| {
            S3Error::Body("a truncated listing carries no continuation token".to_string())
        })?)
    } else {
        None
    };
    Ok(ListPage { objects, next })
}

/// The region of a virtual-hosted S3 base, `https://<bucket>.s3.<region>.amazonaws.com`, and the
/// bucket: `None` for any other shape.
pub fn region_and_bucket(base: &str) -> Option<(String, String)> {
    let host = base.strip_prefix("https://")?;
    let (bucket, rest) = host.split_once(".s3.")?;
    let region = rest.strip_suffix(".amazonaws.com")?;
    let ok = |s: &str| !s.is_empty() && !s.contains(['/', ':']);
    (ok(bucket) && ok(region) && !region.contains('.'))
        .then(|| (region.to_string(), bucket.to_string()))
}

#[cfg(test)]
#[path = "s3_tests.rs"]
mod tests;
