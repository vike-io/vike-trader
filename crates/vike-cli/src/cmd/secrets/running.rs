//! **Tell the running gatewayhub that an account's credentials changed** (decision 0287).
//!
//! After `secrets set`, `secrets rm` or `account copy-credentials` has LANDED a write on an account, the verb
//! asks the datahub to relay the account's ROW ID to the gatewayhub, which reconnects that account's adapter, and no
//! other, on what the store holds now: no daemon restarts, no other venue or account is touched, and no resting order
//! is cancelled (the account's engine sees its link drop once and come back). It is event-driven: nothing polls the
//! store for it.
//!
//! * **Only the row id travels**, on the wire, and never on argv: the gatewayhub re-reads the store itself, so no value
//!   leaves this process by this path, and nothing here prints or logs one.
//! * **Through the datahub, never a dial of the gatewayhub**: this program links no gatewayhub client (decision 0253),
//!   so the hub that holds the gatewayhub's control key relays it, signed with this box's device key (decision 0288).
//! * **The write has already landed, so nothing here fails the verb.** Every outcome is ONE line: what the running
//!   gatewayhub did, or why none was told, in which case the account reads the new value at its next start.

use vike_datahub_client::DatahubClient;
use vike_datahub_client::gateway_status::CredentialsChangedReply;
use vike_hub_protocol::auth::{HubKeys, Scope};

/// Print what the running gatewayhub did for each of `accounts` (the context's closure over [`tell`]), one line each,
/// AFTER the write landed; nothing when the context tells nobody.
pub(super) fn tell_each(ctx: &super::Context<'_>, accounts: impl IntoIterator<Item = i64>) {
    let Some(tell) = ctx.tell_gatewayhub else { return };
    for account in accounts {
        println!("{}", tell(account));
    }
}

/// Ask the datahub at `datahub_addr` (`config.datahub_addr`, else the default) to tell the gatewayhub that account row
/// `account`'s credentials changed, under `keys` (this box's device key, `None` on a box that holds none), and say
/// what happened in one line.
pub(crate) fn tell(datahub_addr: Option<&str>, keys: Option<&HubKeys>, account: i64) -> String {
    let addr = vike_client::resolved::datahub_dial_address(None, datahub_addr);
    let answer = DatahubClient::connect_with_keys(&addr, keys, Scope::Write)
        .map_err(|e| format!("no datahub answered at {addr} ({e}), and only a datahub relays it"))
        .and_then(|mut client| client.credentials_changed(account));
    line(account, answer)
}

/// The one line [`tell`] prints for `account`, from what the datahub answered.
fn line(account: i64, answer: Result<CredentialsChangedReply, String>) -> String {
    let later = "the new credentials take effect at the account's next start";
    match answer {
        Ok(CredentialsChangedReply { error: None, reconnected, .. }) if reconnected.is_empty() => {
            format!("the running gatewayhub runs no session of account {account}: {later}")
        }
        Ok(CredentialsChangedReply { error: None, reconnected, .. }) => format!(
            "the running gatewayhub reconnected {} on the new credentials: each sees its link drop once \
             and come back; no other account was touched and no resting order was cancelled",
            reconnected.join(", ")
        ),
        Ok(CredentialsChangedReply { reached: false, error: Some(why), .. }) => {
            format!("no gatewayhub is running ({why}): {later}")
        }
        Ok(CredentialsChangedReply { error: Some(why), .. }) | Err(why) => {
            format!("the running gatewayhub was not told ({why}): {later}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(reached: bool, reconnected: &[&str], error: Option<&str>) -> CredentialsChangedReply {
        CredentialsChangedReply {
            reached,
            reconnected: reconnected.iter().map(|s| (*s).to_string()).collect(),
            error: error.map(str::to_string),
        }
    }

    /// Every outcome is one line, and only a reconnect says the account changed now.
    #[test]
    fn each_answer_is_one_line_and_only_a_reconnect_says_it_took_effect_now() {
        let now = line(7, Ok(reply(true, &["dukascopy"], None)));
        assert!(now.contains("reconnected dukascopy") && now.contains("no resting order"), "{now}");
        for later in [
            line(7, Ok(reply(true, &[], None))),
            line(7, Ok(reply(false, &[], Some("gatewayhub 127.0.0.1:1: cannot connect")))),
            line(7, Ok(reply(true, &[], Some("gatewayhub does not advertise it")))),
            line(7, Err("no datahub answered at 127.0.0.1:1".to_string())),
        ] {
            assert!(later.ends_with("take effect at the account's next start"), "{later}");
            assert!(!later.contains('\n'), "{later}");
        }
        assert!(line(7, Ok(reply(false, &[], Some("x")))).starts_with("no gatewayhub is running"));
    }

    /// A box with nothing listening is told so, at once, and the verb goes on.
    #[test]
    fn a_datahub_that_does_not_answer_is_a_line_not_a_failure() {
        let said = tell(Some("127.0.0.1:1"), None, 7);
        assert!(said.contains("no datahub answered at 127.0.0.1:1"), "{said}");
        assert!(said.ends_with("next start"), "{said}");
    }
}
