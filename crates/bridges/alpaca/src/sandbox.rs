//! Sandbox-only account provisioning (Broker API): create + fake-fund a test account so smokes and
//! one-time setup can mint the pinned `account_id`. Refuses to run against a live environment.

use vike_bridge_core::credentials::Environment;

use crate::config::AlpacaConfig;
use crate::rest::{AlpacaApiError, AlpacaRest};

/// The `/v1/accounts` POST body. `ssn` must be non-sequential, area != 000/666 (Alpaca validates).
pub fn build_account_body(email: &str, ssn: &str) -> serde_json::Value {
    serde_json::json!({
        "contact": {
            "email_address": email, "phone_number": "+15556667788",
            "street_address": ["123 Test St"], "city": "San Francisco",
            "state": "CA", "postal_code": "94103", "country": "USA"
        },
        "identity": {
            "given_name": "Vike", "family_name": "Tester", "date_of_birth": "1985-06-15",
            "tax_id": ssn, "tax_id_type": "USA_SSN", "country_of_citizenship": "USA",
            "country_of_birth": "USA", "country_of_tax_residence": "USA",
            "funding_source": ["employment_income"]
        },
        "disclosures": {
            "is_control_person": false, "is_affiliated_exchange_or_finra": false,
            "is_politically_exposed": false, "immediate_family_exposed": false
        },
        "agreements": [
            {"agreement": "customer_agreement", "signed_at": "2026-07-14T00:00:00Z", "ip_address": "127.0.0.1"}
        ]
    })
}

/// Create a sandbox test account, returning its `id`. Refuses on a live environment.
pub fn create_test_account(
    rest: &AlpacaRest,
    config: &AlpacaConfig,
    email: &str,
    ssn: &str,
) -> Result<String, AlpacaApiError> {
    if config.env == Environment::Live {
        return Err(AlpacaApiError {
            status: 0,
            message: "refusing to create a test account on LIVE".into(),
        });
    }
    let resp =
        rest.post_json(config.hosts.broker, "/v1/accounts", &build_account_body(email, ssn))?;
    resp.get("id")
        .and_then(|i| i.as_str())
        .map(str::to_string)
        .ok_or_else(|| AlpacaApiError { status: 0, message: "no account id in response".into() })
}

/// Fake-fund a sandbox account (incoming ACH). Exact wire body iterated by the Task 9 smoke.
pub fn fund_test_account(
    rest: &AlpacaRest,
    config: &AlpacaConfig,
    account_id: &str,
    amount: f64,
) -> Result<serde_json::Value, AlpacaApiError> {
    if config.env == Environment::Live {
        return Err(AlpacaApiError { status: 0, message: "refusing to fund on LIVE".into() });
    }
    let body = serde_json::json!({ "transfer_type": "ach", "direction": "INCOMING", "amount": format!("{amount}") });
    rest.post_json(config.hosts.broker, &format!("/v1/accounts/{account_id}/transfers"), &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_body_has_required_sections() {
        let b = build_account_body("vike.test@example.com", "587-24-9310");
        assert_eq!(b["contact"]["email_address"], "vike.test@example.com");
        assert_eq!(b["identity"]["tax_id"], "587-24-9310");
        assert!(
            b["agreements"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["agreement"] == "customer_agreement")
        );
        assert_eq!(b["disclosures"]["is_control_person"], false);
    }
}
