//! The perp start's position-mode check (`VenueRestAsterPerp::check_position_mode`), offline: a stub transport answers
//! `GET /fapi/v3/positionSide/dual`. A hedge account reads `Hedge` (said at `error` with the hand fix), a one-way
//! one `OneWay`, and an answer that is not a mode, or a failed read, `Unknown`; in every case the check makes ONE
//! read and NO write (the bridge switches no aster account's mode). NO socket: the transport is a stub.

use std::sync::Mutex;

use vike_aster::execution::rest::perp::{PATH_POSITION_MODE, VenueRestAsterPerp};
use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::{PositionMode, SymbolProperties};

struct NullSigner;
impl Signer for NullSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

/// Answers every signed request with `answer`, recording `(method, path)`.
struct ModeStub {
    answer: Result<serde_json::Value, i64>,
    seen: Mutex<Vec<(String, String)>>,
}
impl RestTransport for ModeStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        self.seen.lock().unwrap().push((method.to_string(), path.to_string()));
        self.answer.clone().map_err(|code| VenueApiError { code, msg: "stub".into() })
    }
    vike_bridge_core::no_public_call!();
}

fn checked(answer: Result<serde_json::Value, i64>) -> (PositionMode, Vec<(String, String)>) {
    let rest = VenueRestAsterPerp {
        builder: None,
        signer: NullSigner,
        transport: ModeStub { answer, seen: Mutex::new(Vec::new()) },
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: SymbolProperties::default(),
        leverage: 2.0,
    };
    let mode = rest.check_position_mode();
    (mode, rest.transport.seen.into_inner().unwrap())
}

#[test]
fn the_start_reads_the_position_mode_once_and_never_writes_it() {
    let read = vec![("GET".to_string(), PATH_POSITION_MODE.to_string())];
    let cases = [
        (Ok(serde_json::json!({"dualSidePosition": true})), PositionMode::Hedge),
        (Ok(serde_json::json!({"dualSidePosition": false})), PositionMode::OneWay),
        (Ok(serde_json::json!({"dualSidePosition": "true"})), PositionMode::Hedge),
        (Ok(serde_json::json!({"msg": "no mode here"})), PositionMode::Unknown),
        (Err(-1000), PositionMode::Unknown),
    ];
    for (answer, want) in cases {
        let label = format!("{answer:?}");
        let (mode, seen) = checked(answer);
        assert_eq!(mode, want, "{label}");
        assert_eq!(seen, read, "one GET of the mode and no write: {label}");
    }
}
