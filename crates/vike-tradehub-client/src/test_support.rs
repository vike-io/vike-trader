//! The negotiation suites' scripted node and the strategy-plane script over it, compiled ONCE here
//! behind the `test-support` feature instead of once per suite that used to `#[path]`-include
//! `tests/support/`, plus the [`CommandWire`](crate::wire::CommandWire) variant names the two
//! capability-projection gates (vike-tradehub's and vike-cli's) both classify, and the wire commands
//! (`commands`) this crate's and the tradehub's suites both send. This crate's own suites name them as
//! `vike_tradehub_client::test_support::{commands, fake_node, strategy_node}`; the two gates reach
//! `wire_command_names` through their dev edges. [`canned_balance`] is the Polymarket balance read the
//! venue-read suites (this crate's negotiation suite and vike-cli's `trade venue` renderer) answer with.
//! `observe_frame` is the one fully populated observe frame the decode suites (this crate's and the desktop's)
//! fold through `observe_decode::wire_to_core`. [`assert_round_trips_in_every_form`] is the wire suites' one check of a
//! message through every session form.

pub mod commands;
pub mod fake_node;
pub mod observe_frame;
pub mod strategy_node;
pub mod wire_command_names;

use vike_hub_protocol::frame::{BodyCodec, decode_body, encode_frame};
use vike_model::{VenueRead, VenueReadAllowance, VenueReadBalance};

/// One message through every session form (`vike_hub_protocol::frame::BodyCodec`: JSON, msgpack, its LZ4 form): each
/// reads back equal through the reader every peer uses, and what a msgpack copy reads back prints as the very JSON the
/// JSON copy does, so a printer at the edge (the CLI's `--json`, MCP's side of `vike-mcp`) writes the same bytes
/// whichever form carried it.
pub fn assert_round_trips_in_every_form<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let printed = serde_json::to_vec(value).expect("json");
    for codec in [BodyCodec::Json, BodyCodec::Msgpack, BodyCodec::MsgpackLz4] {
        // A request's form: LZ4 always, so every form is exercised.
        let body = encode_frame(codec, value).expect("encode")[4..].to_vec();
        assert_eq!(BodyCodec::of(&body), codec, "{value:?}");
        let back: T = decode_body(&body).unwrap_or_else(|e| panic!("{codec:?} {value:?}: {e}"));
        assert_eq!(&back, value, "{codec:?}");
        assert_eq!(serde_json::to_vec(&back).expect("json"), printed, "{codec:?}: printed");
    }
}

/// A Polymarket balance read: 20 pUSD, one unlimited allowance to the CTF exchange at `spender` (a
/// renderer test passes a short one it can read back, a wire test the real contract address).
pub fn canned_balance(spender: &str) -> VenueRead {
    VenueRead::Balance(VenueReadBalance {
        asset: "pUSD".into(),
        balance: 20.0,
        allowances: vec![VenueReadAllowance {
            spender: spender.into(),
            name: "CTF exchange".into(),
            amount: 0.0,
            unlimited: true,
        }],
        other_assets: Vec::new(),
    })
}
