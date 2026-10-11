//! The Connections panel driven through the REAL widget (`vike_connections::connections_ui`)
//! under egui_kittest, as ONE test binary: the accessibility tree (`a11y_*`), the layout, the
//! account editor, the selection colour and the Save arm's change journal. `support` holds the
//! helpers more than one suite uses.

#[path = "panel/a11y_detail.rs"]
mod a11y_detail;
#[path = "panel/a11y_form.rs"]
mod a11y_form;
#[path = "panel/a11y_secrets.rs"]
mod a11y_secrets;
#[path = "panel/account_editor.rs"]
mod account_editor;
#[path = "panel/layout.rs"]
mod layout;
#[path = "panel/selection_colour.rs"]
mod selection_colour;
#[path = "panel/support.rs"]
mod support;
#[path = "panel/write_journal.rs"]
mod write_journal;
