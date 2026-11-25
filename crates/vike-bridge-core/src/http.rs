//! The rate-limit-aware raw GET of the kline backfill modules (`data.rs`): [`get_raw`] + [`RawResponse`] capture the
//! status + `Retry-After` (+ the Binance-family weight header) BEFORE draining the body, so the per-venue retry loops
//! can classify and back off. The response *classification* stays per-venue (which statuses/business codes mean rate
//! limited genuinely differ), and so do the PAGING loops (forward vs backward). The backoff *cadence* is the shared
//! [`crate::retry::retry_rate_limited`] driver (the family's page `used_weight` rides out via the `Verdict::Done`
//! payload).
//!
//! The agents it rides, the keyless public read (`get_json`) and the body bound are the generic network layer's,
//! `vike_transport::http` (decision 0299); this module keeps only the pager's venue-shaped read.

use std::time::Duration;

use vike_transport::http::read_body_text;

/// A raw HTTP result captured before the body is interpreted: the status, the rate-limit-relevant
/// headers, and the body.
///
/// `used_weight` is the Binance-family `X-MBX-USED-WEIGHT-1M` (binance/aster read it to cool down
/// proactively); it is simply `None` on venues that don't send it, which those callers ignore.
pub struct RawResponse {
    pub status: u16,
    pub retry_after: Option<Duration>,
    pub used_weight: Option<u64>,
    pub body: String,
}

/// One blocking GET capturing status + the rate-limit headers before draining the body.
///
/// `label` prefixes the two error messages (`"{label} GET: …"` / `"{label} read: …"`), keeping each
/// venue's wording. `extra_headers` are set on the request (OKX passes its Accept; the agent
/// names this client). The shared agent has `http_status_as_error(false)`, so 429/418/403 arrive
/// here as a `status`, NOT as a transport error — which is what lets the caller's retry loop honor
/// `Retry-After` instead of failing the page.
pub fn get_raw(
    agent: &ureq::Agent,
    url: &str,
    label: &str,
    extra_headers: &[(&str, &str)],
) -> Result<RawResponse, String> {
    let mut req = agent.get(url);
    for (name, value) in extra_headers {
        req = req.header(*name, *value);
    }
    let mut response = req.call().map_err(|e| format!("{label} GET: {e}"))?;
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let used_weight = response
        .headers()
        .get("x-mbx-used-weight-1m")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok());
    let body = read_body_text(response.body_mut()).map_err(|e| format!("{label} read: {e}"))?;
    Ok(RawResponse { status, retry_after, used_weight, body })
}

#[cfg(test)]
mod response_body_limit_tests {
    use super::get_raw;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use vike_transport::http::{RESPONSE_BODY_LIMIT, blocking_agent};

    /// A gzip of `len` bytes of `x`, small on the wire: one fixed-Huffman deflate block of a literal
    /// and back-references of 258 at distance 1 (RFC 1951 3.2.6), so a 12 MiB body travels as ~80 KB
    /// and a 64 MiB one as ~420 KB. Built by hand because this crate has no deflate dependency.
    fn gzip_of_x(len: usize) -> Vec<u8> {
        struct Bits {
            out: Vec<u8>,
            pending: u64,
            count: u32,
        }
        impl Bits {
            /// `count` bits of `value`, least significant first (block header and extra bits).
            fn put(&mut self, value: u64, count: u32) {
                self.pending |= value << self.count;
                self.count += count;
                while self.count >= 8 {
                    self.out.push(self.pending as u8);
                    self.pending >>= 8;
                    self.count -= 8;
                }
            }
            /// A Huffman code, most significant bit first.
            fn code(&mut self, code: u64, length: u32) {
                let reversed =
                    (0..length).fold(0, |r, i| r | (((code >> i) & 1) << (length - 1 - i)));
                self.put(reversed, length);
            }
        }
        let mut bits =
            Bits { out: vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255], pending: 0, count: 0 };
        bits.put(1, 1); // BFINAL
        bits.put(1, 2); // BTYPE 01: fixed Huffman codes
        let literal_x = |bits: &mut Bits| bits.code(0x30 + u64::from(b'x'), 8);
        literal_x(&mut bits);
        let mut left = len - 1;
        while left >= 258 {
            bits.code(0xC5, 8); // length 258 (symbol 285)
            bits.code(0, 5); // distance 1
            left -= 258;
        }
        (0..left).for_each(|_| literal_x(&mut bits));
        bits.code(0, 7); // end of block (symbol 256)
        let mut out = bits.out;
        if bits.count > 0 {
            out.push(bits.pending as u8); // the last partial byte, zero-padded
        }
        // CRC-32 (the gzip trailer), by table: 64 MiB is a few hundred ms in a debug build.
        let table: Vec<u32> = (0..256u32)
            .map(|n| (0..8).fold(n, |c, _| (c >> 1) ^ (0xEDB8_8320 & (c & 1).wrapping_neg())))
            .collect();
        let crc =
            (0..len).fold(!0u32, |c, _| table[((c ^ u32::from(b'x')) & 0xff) as usize] ^ (c >> 8));
        out.extend_from_slice(&(!crc).to_le_bytes());
        out.extend_from_slice(&(len as u32).to_le_bytes());
        out
    }

    /// One response, `Content-Encoding: gzip`, carrying `gzip`.
    fn serve_gzip(gzip: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let Ok((mut socket, _)) = listener.accept() else { return };
            let mut buffer = [0u8; 2048];
            let _ = socket.read(&mut buffer);
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-encoding: gzip\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                gzip.len()
            );
            // The over-limit read stops early and closes: a failed write is that, not a defect.
            let _ = socket.write_all(head.as_bytes()).and_then(|()| socket.write_all(&gzip));
        });
        format!("http://{addr}/list")
    }

    /// A gzip list that decodes past ureq's 10 MiB default and under [`RESPONSE_BODY_LIMIT`] (Binance's
    /// spot `exchangeInfo` is ~18 MB decoded) is read whole: ureq 3.5.2 counts the DECODED bytes.
    /// MUTATION PROOF: read with a bare `read_to_string()` in `read_body_text` — this fails at 10 MiB.
    #[test]
    fn a_gzip_list_past_ten_mebibytes_decoded_is_read_whole() {
        let len = 12 * 1024 * 1024;
        let url = serve_gzip(gzip_of_x(len));
        let reply = get_raw(&blocking_agent(), &url, "list", &[]).expect("the list is read");
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body.len(), len);
        assert!(reply.body.bytes().all(|b| b == b'x'), "the body decoded to what was sent");
    }

    /// ...and one byte past the limit is refused, however small it is on the wire (~420 KB here): the
    /// limit is the bound on a decompression bomb.
    #[test]
    fn a_body_past_the_limit_decoded_is_refused() {
        let url = serve_gzip(gzip_of_x(RESPONSE_BODY_LIMIT as usize + 1));
        let Err(refused) = get_raw(&blocking_agent(), &url, "list", &[]) else {
            panic!("a body past the limit was read whole");
        };
        assert!(refused.contains("larger than request limit"), "the limit refused it: {refused}");
    }
}
