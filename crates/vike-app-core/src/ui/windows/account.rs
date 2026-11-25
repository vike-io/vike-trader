//! The Account window's glue: the body is `vike_account_window`'s, which takes its inputs as
//! a value and RETURNS the History read it wants; this builds those inputs off [`WindowToolContext`],
//! hands it the window's state (`WindowView::account`: its tab, the History tab's view and the cancel
//! slot the shell drains into the dispatcher) off the `&mut WindowView`, and hands the read to
//! [`crate::data::execution_history::spawn_load`] over the shell's dial ([`AccountWindowHistoryContext`]),
//! under this crate's [`RPC_DEADLINE`].

use super::WindowToolContext;
use crate::data::data_manager::RPC_DEADLINE;
use crate::data::execution_history;
use crate::ui::windows::WindowView;
use vike_account_window::{AccountWindowInputs, account_window_body};

/// The read-only inputs the Account window's History read takes from [`WindowToolContext`]: where the
/// datahub is, and the SHELL's dial (`fn(address)`), because resolving this PC's device key needs the
/// binary's settings directory, which this crate does not read.
#[derive(Clone, Copy)]
pub struct AccountWindowHistoryContext<'a> {
    pub datahub_address: Option<&'a str>,
    pub dial: fn(String) -> vike_datahub_client::HistoryStoreDatahub,
}

impl<'a> AccountWindowHistoryContext<'a> {
    pub fn new(
        datahub_address: Option<&'a str>,
        dial: fn(String) -> vike_datahub_client::HistoryStoreDatahub,
    ) -> Self {
        Self { datahub_address, dial }
    }

    /// No datahub: the window says so and dials nothing.
    pub fn none() -> AccountWindowHistoryContext<'static> {
        AccountWindowHistoryContext { datahub_address: None, dial: dial_without_keys }
    }
}

fn dial_without_keys(address: String) -> vike_datahub_client::HistoryStoreDatahub {
    vike_datahub_client::HistoryStoreDatahub::new(address)
}

/// The Account window (`vike_account_window::account_window_body`) over this window's view,
/// and the History read it asks for handed to a background thread over the shell's dial.
pub fn account_tool_content(
    ui: &mut egui::Ui,
    context: &WindowToolContext<'_>,
    tv: &mut WindowView,
) {
    let inputs = AccountWindowInputs {
        snapshot: context.snapshot,
        words: context.node_words,
        datahub: context.execution_history.datahub_address,
        display_time_zone: context.display_time_zone,
        directory: context.directory,
        now_ms: vike_model::now_ms(),
    };
    if let Some(query) = account_window_body(ui, &inputs, &mut tv.account) {
        let (address, dial) = (
            context.execution_history.datahub_address.map(str::to_string),
            context.execution_history.dial,
        );
        let wake = ui.ctx().clone();
        execution_history::spawn_load(
            query,
            tv.account.history.slot(),
            RPC_DEADLINE,
            move || address.clone().map(dial),
            move || wake.request_repaint(),
        );
    }
}
