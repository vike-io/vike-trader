//! The Data pane's CSV UPLOAD (decision 0272): read the user's file on a worker thread, send it to the
//! datahub with the OBSERVE key this desktop already holds, and hand back each chunk's progress and the
//! answer. The datahub decodes and stores; this computes nothing of the file (decision 0078 as amended
//! 2026-11-10): it reads the bytes, cuts a preview's head on a character, and sends them.
//!
//! The wire is `vike_datahub_client::upload`'s, and the verb is `DatahubClient::upload_csv`; this file
//! only runs it off the paint thread.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use vike_datahub_client::DatahubClient;
use vike_datahub_client::archive::ImportTarget;
use vike_datahub_client::upload::{CsvUploadDone, file_head, file_text};
use vike_hub_protocol::auth::{HubKeys, Scope};

/// Where an upload goes: the datahub the Studio's store reads, and the OBSERVE key it reads with —
/// never a control key (`vike_app_core::backend::backend_registry`'s `datahub_observe_key` says why),
/// which the upload does not need. Seeded by the desktop when the Studio's store is remote.
#[derive(Clone)]
pub struct CsvUploadDial {
    pub addr: String,
    pub keys: Option<HubKeys>,
}

impl std::fmt::Debug for CsvUploadDial {
    /// The address and whether a key is held: never the key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CsvUploadDial")
            .field("addr", &self.addr)
            .field("keyed", &self.keys.is_some())
            .finish()
    }
}

/// What the worker says while an upload runs: each acknowledged chunk, then the end.
#[derive(Debug)]
pub enum UploadEvent {
    /// The bytes the datahub has received of the text being sent.
    Progress { sent: u64, total: u64 },
    /// How the datahub read the file and what it stored, or why the upload was refused or broke off.
    Done(Result<CsvUploadDone, String>),
}

/// Send the file at `path` as `target`'s series on a worker thread: a PREVIEW (`preview`) sends its
/// head as a dry run and stores nothing; otherwise the whole file is sent and stored. The receiver
/// yields [`UploadEvent::Progress`] per chunk and one [`UploadEvent::Done`]; `ctx` is asked to repaint
/// after each.
pub fn spawn_csv_upload(
    dial: CsvUploadDial,
    path: PathBuf,
    target: ImportTarget,
    preview: bool,
    ctx: &egui::Context,
) -> Receiver<UploadEvent> {
    let (tx, rx) = channel();
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name("csv-upload".into())
        .spawn(move || {
            let done = run(&dial, &path, &target, preview, &mut |sent, total| {
                let _ = tx.send(UploadEvent::Progress { sent, total });
                ctx.request_repaint();
            });
            let _ = tx.send(UploadEvent::Done(done));
            ctx.request_repaint();
        })
        .expect("spawn csv-upload thread");
    rx
}

fn run(
    dial: &CsvUploadDial,
    path: &std::path::Path,
    target: &ImportTarget,
    preview: bool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<CsvUploadDone, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let (text, head) = if preview {
        let (text, whole) = file_head(&bytes)?;
        (text, !whole)
    } else {
        (file_text(&bytes)?, false)
    };
    let mut client = DatahubClient::connect_with_keys(&dial.addr, dial.keys.as_ref(), Scope::Read)
        .map_err(|e| format!("cannot reach the datahub at {}: {e}", dial.addr))?;
    client.upload_csv(target, text, preview, head, progress)
}
