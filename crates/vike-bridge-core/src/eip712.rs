//! Minimal EIP-712 typed-data hashing + secp256k1 signing — the generic primitive shared by
//! venue bridges that sign with an EVM wallet key (Polymarket L1/order auth, Aster v3 request
//! auth). Only FLAT structs (address / uintN / string / bytes fields) are supported. Validated
//! against the canonical EIP-712 spec vectors in the test module below. Gated behind the
//! `eip712` Cargo feature (needs `k256` + `tiny-keccak`).

use k256::ecdsa::{RecoveryId, Signature, SigningKey};
use tiny_keccak::{Hasher, Keccak};

/// keccak256 (Ethereum's hash — NOT SHA3-256).
pub fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut k = Keccak::v256();
    k.update(data);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    out
}

fn hex_to_bytes(s: &str) -> Vec<u8> {
    hex::decode(s.strip_prefix("0x").unwrap_or(s)).unwrap_or_default()
}

/// Encode an `0x…` address as a 32-byte word (20 bytes right-aligned).
pub fn enc_address(addr: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let b = hex_to_bytes(addr);
    let n = b.len().min(20);
    out[32 - n..].copy_from_slice(&b[b.len() - n..]);
    out
}

/// Encode a `uintN` value as a 32-byte big-endian word.
pub fn enc_uint(v: u128) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[16..].copy_from_slice(&v.to_be_bytes());
    out
}

/// Encode a dynamic `string`/`bytes` field: keccak256 of its bytes.
pub fn enc_string(s: &str) -> [u8; 32] {
    keccak256(s.as_bytes())
}

/// Encode a decimal `uint256` string (e.g. an ERC-1155 tokenId, too big for u128) as a 32-byte
/// big-endian word. Manual base-256 accumulation — no bigint dependency. Non-digits are ignored;
/// values wider than 256 bits wrap (caller supplies valid uint256 decimals).
pub fn enc_uint256_dec(dec: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for ch in dec.bytes() {
        if !ch.is_ascii_digit() {
            continue;
        }
        let mut carry = u16::from(ch - b'0');
        for byte in out.iter_mut().rev() {
            let v = u16::from(*byte) * 10 + carry;
            *byte = (v & 0xff) as u8;
            carry = v >> 8;
        }
    }
    out
}

/// `hashStruct(s) = keccak256( typeHash(encodeType) || enc(field0) || … )` — flat structs only.
pub fn hash_struct(encode_type: &str, fields: &[[u8; 32]]) -> [u8; 32] {
    let mut buf = Vec::with_capacity(32 * (fields.len() + 1));
    buf.extend_from_slice(&keccak256(encode_type.as_bytes()));
    for f in fields {
        buf.extend_from_slice(f);
    }
    keccak256(&buf)
}

/// The standard `EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)`
/// separator.
pub fn domain_separator(
    name: &str,
    version: &str,
    chain_id: u128,
    verifying_contract: &str,
) -> [u8; 32] {
    hash_struct(
        "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
        &[
            enc_string(name),
            enc_string(version),
            enc_uint(chain_id),
            enc_address(verifying_contract),
        ],
    )
}

/// The `EIP712Domain(string name,string version,uint256 chainId)` separator — NO verifyingContract
/// (Polymarket's ClobAuth L1 domain omits it, unlike the Order domain).
pub fn domain_separator_no_contract(name: &str, version: &str, chain_id: u128) -> [u8; 32] {
    hash_struct(
        "EIP712Domain(string name,string version,uint256 chainId)",
        &[enc_string(name), enc_string(version), enc_uint(chain_id)],
    )
}

/// The final signable digest: `keccak256( 0x1901 || domainSeparator || hashStruct(message) )`.
pub fn digest(domain_sep: &[u8; 32], struct_hash: &[u8; 32]) -> [u8; 32] {
    let mut buf = Vec::with_capacity(66);
    buf.extend_from_slice(&[0x19, 0x01]);
    buf.extend_from_slice(domain_sep);
    buf.extend_from_slice(struct_hash);
    keccak256(&buf)
}

/// Sign a 32-byte digest with a secp256k1 private key; returns Ethereum `r||s||v` (65 bytes,
/// v = 27 + recovery id, low-S normalized by k256).
pub fn sign_digest(digest: &[u8; 32], private_key_hex: &str) -> Result<[u8; 65], String> {
    let sk = SigningKey::from_slice(&hex_to_bytes(private_key_hex)).map_err(|e| e.to_string())?;
    let (sig, recid): (Signature, RecoveryId) = sk.sign_prehash_recoverable(digest);
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = recid.to_byte() + 27;
    Ok(out)
}

/// Sign as a hex `0x…` string (what Polymarket's order/auth payloads carry).
pub fn sign_digest_hex(digest: &[u8; 32], private_key_hex: &str) -> Result<String, String> {
    Ok(format!("0x{}", hex::encode(sign_digest(digest, private_key_hex)?)))
}

/// Derive the Ethereum EOA address (`0x…`, lowercase) from a secp256k1 private key:
/// `keccak256(uncompressed_pubkey[1..])[12..]`.
pub fn eth_address_from_private_key(private_key_hex: &str) -> Result<String, String> {
    let sk = SigningKey::from_slice(&hex_to_bytes(private_key_hex)).map_err(|e| e.to_string())?;
    let point = sk.verifying_key().to_sec1_point(false); // 0x04 || X(32) || Y(32)
    let hash = keccak256(&point.as_bytes()[1..]); // hash X||Y (drop the 0x04 tag)
    Ok(format!("0x{}", hex::encode(&hash[12..])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keccak_known_vector() {
        assert_eq!(
            hex::encode(keccak256(b"abc")),
            "4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45"
        );
    }

    #[test]
    fn eip712_domain_separator_ether_mail() {
        // Canonical EIP-712 spec example: domain {Ether Mail, 1, chainId 1, 0xCcCc…cccC}
        let sep =
            domain_separator("Ether Mail", "1", 1, "0xCcCCccccCCCCcCCCCCCcCcCccCcCCCcCcccccccC");
        assert_eq!(
            hex::encode(sep),
            "f2cee375fa42b42143804025fc449deafd50cc031ca257e0b194a650a912090f"
        );
    }

    #[test]
    fn sign_reference_digest() {
        // Canonical EIP-712 "Mail" final digest + the spec's example private key → known r,s,v.
        let mut d = [0u8; 32];
        d.copy_from_slice(
            &hex::decode("be609aee343fb3c4b28e1df9e632fca64fcfaede20f02e86244efddf30957bd2")
                .unwrap(),
        );
        // the spec's "Cow" signer key = keccak256("cow")
        let sig =
            sign_digest(&d, "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4")
                .unwrap();
        let hex_sig = hex::encode(sig);
        assert_eq!(
            &hex_sig[..64],
            "4355c47d63924e8a72e509b65029052eb6c299d53a04e167c5775fd466751c9d"
        );
        assert_eq!(
            &hex_sig[64..128],
            "07299936d304c153f6443dfa05f40ff007d72911b6f72307f996231605b91562"
        );
        assert_eq!(&hex_sig[128..130], "1c"); // v = 28
    }

    #[test]
    fn uint256_dec_encoding() {
        assert_eq!(hex::encode(enc_uint256_dec("0")), "0".repeat(64));
        assert_eq!(&hex::encode(enc_uint256_dec("1"))[62..], "01");
        assert_eq!(&hex::encode(enc_uint256_dec("255"))[62..], "ff");
        assert_eq!(&hex::encode(enc_uint256_dec("256"))[60..], "0100");
        // 2^128 sits exactly at the byte-16 boundary
        assert_eq!(
            hex::encode(enc_uint256_dec("340282366920938463463374607431768211456")),
            "0000000000000000000000000000000100000000000000000000000000000000"
        );
    }

    #[test]
    fn eth_address_from_cow_key() {
        // the spec's "Cow" key → the known Cow EOA address
        let addr = eth_address_from_private_key(
            "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4",
        )
        .unwrap();
        assert_eq!(addr, "0xcd2a3d9f938e13cd947ec05abc7fe734df8dd826");
    }
}
