//! The `datahub` and `gatewayhub` rows of [`super::judge`]: the datahub's word on the gatewayhub, and a node
//! advertising another datahub than the one asked. The words of each row are in the parent's module doc.

use super::*;

/// What the rest of the report can say for the two hubs, which is what `hub_checks` may lean on and point at.
#[derive(Clone, Copy)]
pub(super) struct Beside {
    pub(super) door_answered: bool,
    /// A `gateway-link` row judges a lease here, or nothing here wants the gatewayhub at all.
    pub(super) needs_no_gatewayhub_row: bool,
    /// A `price-link` row reads the datahub from the tradehub's side.
    pub(super) price_link_reads: bool,
}
impl Beside {
    /// A door that gave no engine to judge: no other row speaks for either hub.
    pub(super) fn no_rows(door_answered: bool) -> Self {
        Self { door_answered, needs_no_gatewayhub_row: false, price_link_reads: false }
    }
}
/// `datahub` and `gatewayhub`: what the datahub says of the gatewayhub. The rule is to name the hop and never claim past
/// it, so what stands beside these rows decides two words:
///
/// * The door answered and the datahub cannot be dialled from here: UNKNOWN, because a tunnel not forwarded reads the same
///   as a dead daemon. It points at `price-link` only where that row reads the datahub (at least one dial holds a session).
///   With the door silent too, FAIL.
/// * The datahub cannot reach the gatewayhub, and a lease's `gateway-link` row judges the orders (links that are up prove
///   the daemon alive whatever the datahub's own hop says), or nothing here wants it: WARN `datahub DEGRADED`. Otherwise
///   nothing else can speak for it (a silent or a starting door, a pre-fold one, a box whose only engines are paper beside
///   an account that failed to start): FAIL `gatewayhub`.
///
/// A datahub too old to be asked, or one that refuses this box's key, is UNKNOWN and moves no verdict.
pub(super) fn hub_checks(hubs: Option<&HubLeg>, beside: Beside) -> Vec<Check> {
    let Some(HubLeg { addr, answer }) = hubs else { return Vec::new() };
    let answers = || Check::new(Status::Pass, "datahub", format!("answers at {addr}"));
    let gatewayhub = |status, detail: String| Check::new(status, "gatewayhub", detail);
    let on_its_box = box_status("vike-gatewayhub");
    match answer {
        DatahubAnswer::Refused(why) => vec![Check::new(
            Status::Unknown,
            "datahub",
            format!(
                "unknown: the datahub at {addr} refused this box ({why}), so the gatewayhub and datahub are not \
                 judged: this verb sees them only through the tradehub"
            ),
        )],
        DatahubAnswer::NoAnswer(why) if beside.door_answered => {
            let pointer = if beside.price_link_reads {
                ", and `price-link` is the tradehub's own reading of it"
            } else {
                ""
            };
            vec![Check::new(
                Status::Unknown,
                "datahub",
                format!(
                    "unknown: this client cannot dial the datahub at {addr}: {why}. A dead SSH forward reads the same \
                     as a dead daemon{pointer}. Ask: vike-cli backend ping --addr {addr}"
                ),
            )]
        }
        DatahubAnswer::NoAnswer(why) => vec![
            Check::new(
                Status::Fail,
                "datahub",
                format!("DATAHUB at {addr} does not answer: {why}"),
            ),
            gatewayhub(
                Status::Unknown,
                format!("unknown: no hub can see it. On its box: {on_its_box}"),
            ),
        ],
        DatahubAnswer::TooOld => vec![
            answers(),
            gatewayhub(
                Status::Unknown,
                format!(
                    "unknown (datahub too old): whether the gatewayhub is up, because this datahub does not offer \
                     to say. Ask, on its box: {on_its_box}"
                ),
            ),
        ],
        DatahubAnswer::Said(reply) => {
            let words = said(reply.error.as_deref().unwrap_or_default());
            match &reply.report {
                Some(report) => vec![
                    answers(),
                    gatewayhub(
                        Status::Pass,
                        format!(
                            "up {}, per the datahub ({} connection(s), {} stream session(s), {} fetch(es) running): \
                             the daemon, and this row says nothing of any venue",
                            span(report.uptime_ms),
                            report.connections,
                            report.stream_sessions,
                            report.fetches_running
                        ),
                    ),
                ],
                None if reply.reached => vec![
                    answers(),
                    gatewayhub(
                        Status::Warn,
                        format!("it took the datahub's handshake and gave no report: {words}"),
                    ),
                ],
                None if beside.door_answered && beside.needs_no_gatewayhub_row => vec![Check::new(
                    Status::Warn,
                    "datahub",
                    format!(
                        "DEGRADED: it serves the store, new venue data fails: {words}. Orders do not pass through it: \
                         `gateway-link` judges those"
                    ),
                )],
                None => vec![
                    answers(),
                    gatewayhub(Status::Fail, format!("the datahub cannot reach it: {words}")),
                ],
            }
        }
    }
}
/// The two hub rows read the datahub at THIS client's `config.datahub_addr`, whatever `--node` says, so a node on another
/// box would be judged against the wrong hubs with nothing to show for it. A node that advertises another address says so.
/// UNKNOWN, no verdict: the advertisement is a dial address, and a tunnel to the right datahub differs from it too.
pub(super) fn foreign_datahub_check(
    features: Option<&[String]>,
    hubs: Option<&HubLeg>,
) -> Option<Check> {
    let advertised = advertised_datahub(features?)?;
    let addr = hubs?.addr.trim();
    (advertised != addr).then(|| {
        Check::new(
            Status::Unknown,
            "datahub",
            format!(
                "unknown: these rows asked the datahub at {addr}, this client's `config.datahub_addr`, and this node \
                 advertises {advertised}, so they may describe other hubs than this node's (a tunnel reads the same). \
                 Ask: vike-cli backend ping --addr {advertised}"
            ),
        )
    })
}
