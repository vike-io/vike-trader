//! The datahub's refusals, declared once (`vike_utility::refusal_enum!`) in the crate the daemon and every reader of
//! its wire link, and sent as the refusal frame (`vike_hub_protocol::refusal::HubRefusal`,
//! [`crate::proto::Response::Refused`]) to a client that said it reads one: the ones a client DECIDES on (the
//! strategy lane's cap) and the ones a person READS, the answers of the history reads, the archive import and
//! download, the CSV upload and the series delete (refusal step 3).
//!
//! A string-returning verb of [`crate::DatahubClient`] shows a frame as [`shown`] (`error [<id>]: <text>`); a verb
//! that decides on one reads its id with [`DatahubRefusalId::from_word`]. A store failure is ONE id per kind of
//! [`vike_data::HistoryStoreError`] (`impl From<&HistoryStoreError>`), the store's words a field. A refusal the
//! datahub answers without an id is still `Response::Error`.

use std::io;

use vike_data::HistoryStoreError;
use vike_hub_protocol::refusal::HubRefusal;
use vike_market_stream::opener::StreamOpenCapped;

vike_utility::refusal_enum! {
    /// A datahub refusal: this module's doc.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DatahubRefusal with ids DatahubRefusalId {
        /// `StrategyOpen` met the strategy lane's connection cap (`crates/vike-datahub/src/strat/mod.rs`'s
        /// `StrategyLane::open_session`): a CAP, which only an operator mends. The tradehub reads it as such
        /// (`vike_data::DataClientError::Cap` on every subscribe of that lane) and never restarts on it.
        StrategyLaneFull { cap: usize } => "datahub.strategy_lane.full":
            "strategy lane: already serving {cap} streams (the lane's connection cap): raise the cap or close a \
             stream",
        // ---- the store answered a failure (`impl From<&HistoryStoreError>`), one id per kind ----
        /// A `HistoryStoreError::Query`: the query or a decode failed.
        StoreQuery { why: String } => "datahub.store.query": "hist query: {why}",
        /// A `HistoryStoreError::Io`: the filesystem or a manifest failed.
        StoreIo { why: String } => "datahub.store.io": "hist io: {why}",
        /// A `HistoryStoreError::Unsupported`: this store does not serve the kind asked for.
        StoreUnsupported { why: String } => "datahub.store.unsupported": "hist unsupported: {why}",
        /// A `HistoryStoreError::SupersedeRefused`: worded as a query failure, its own id.
        StoreSupersedeRefused { why: String } => "datahub.store.supersede_refused": "hist query: {why}",
        // ---- the history reads (`crates/vike-datahub/src/server/range_reads.rs`'s `range_rows`) ----
        /// A range read with no `limit` over more rows than one reply carries. `range` is `[<start>, <end>]`, each
        /// end its epoch-ms or `open` (one field, so the refusal stays under clippy's `result_large_err` bound).
        RangeOverCeiling {
            verb: &'static str,
            series: String,
            range: String,
            ceiling: usize,
            unit: &'static str,
            ceiling_name: &'static str,
        } => "datahub.history.range.over_ceiling":
            "{verb} {series} over {range} holds more than {ceiling} {unit} ({ceiling_name}, the most one \
             reply can carry) and this request sent no `limit`, so it is refused and the rest of the range was not \
             read. Send a `limit` and start each next request at the last `ts` + 1 of the reply before it \
             (`HistoryStoreDatahub` pages that way), or narrow the range.",
        /// A range read with no `limit` whose rows are under their ceiling and whose JSON passes the frame.
        RangeOverFrame {
            verb: &'static str,
            series: String,
            range: String,
            unit: &'static str,
            frame_bytes: usize,
            ceiling_name: &'static str,
        } => "datahub.history.range.over_frame":
            "{verb} {series} over {range} is more than one reply can carry: its {unit} pass MAX_FRAME_LEN \
             ({frame_bytes} bytes) of JSON although they number no more than {ceiling_name}, and this request sent \
             no `limit`, so it is refused rather than sent to fail at the frame. Send a `limit` and start each next \
             request at the last `ts` + 1 of the reply before it (`HistoryStoreDatahub` pages that way), or narrow \
             the range.",
        /// A page whose FIRST timestamp's rows alone pass the frame, answered in place of an empty page.
        FirstTimestampOverFrame { verb: &'static str, series: String, unit: &'static str, ts: i64, frame_bytes: usize }
            => "datahub.history.timestamp.over_frame":
            "{verb} {series}: the {unit} at ts {ts} alone pass MAX_FRAME_LEN ({frame_bytes} bytes), so no page can \
             carry them; none is sent, because an empty page would read as the end of the range. This range cannot \
             be read past ts {ts} over this wire.",
        // ---- a bar read resampled (`crates/vike-datahub/src/server/bar_resample.rs`'s `resampled_bars`) ----
        /// A resampled read that sent a `limit`.
        ResampleWithLimit { to: String } => "datahub.history.resample.limit":
            "LoadBars resampled to {to} is answered whole, in one reply: send no `limit`, and narrow the range \
             instead.",
        /// A resampled read with an open end.
        ResampleOpenRange { to: String } => "datahub.history.resample.open_range":
            "LoadBars resampled to {to} must name both ends of its range, so the windows it answers can be counted \
             before anything is read.",
        /// A resample from or to an interval with no measurable width.
        ResampleUnmeasurable { interval: String, to: String } => "datahub.history.resample.unmeasurable":
            "LoadBars cannot resample {interval} to {to}: an interval it cannot measure.",
        /// A resample to a target that is not a whole multiple of the stored interval.
        ResampleNotAMultiple { interval: String, to: String } => "datahub.history.resample.not_a_multiple":
            "LoadBars cannot resample {interval} to {to}: the target must be a whole multiple of the stored \
             interval, so every stored bar falls in one window.",
        /// A resampled range touching more windows than one reply carries.
        ResampleOverWindows { to: String, start: i64, end: i64, windows: i64, ceiling: usize }
            => "datahub.history.resample.over_windows":
            "LoadBars resampled to {to} over [{start}, {end}] touches {windows} windows, more than \
             RESAMPLED_BARS_CEILING ({ceiling}), the most one reply carries. Resample to a coarser interval, or \
             narrow the range.",
        /// A resampled range that reads more stored bars than the scan ceiling.
        ResampleOverScan { interval: String, to: String, start: i64, end: i64, scan_ceiling: usize }
            => "datahub.history.resample.over_scan":
            "LoadBars of {interval} resampled to {to} over [{start}, {end}] reads more than RESAMPLE_SCAN_CEILING \
             ({scan_ceiling}) stored bars, so it was stopped. Narrow the range, or resample a coarser stored \
             interval.",
        /// `HistoryChannels` could not read the store's inventory.
        HistoryChannelsInventory { why: String } => "datahub.history.channels.inventory":
            "HistoryChannels: the store's inventory: {why}",
        // ---- the archive import (`crates/vike-datahub/src/import/mod.rs`'s `import_archive_verb`) ----
        /// A server with no import lane. ⚠ It must keep naming the capability and saying nothing was read
        /// (`crates/vike-datahub-client/tests/archive_import_negotiation.rs` pins both), and name no SCOPE: a
        /// key-less server is not refusing on scope grounds.
        ImportNoLane => "datahub.import.no_lane":
            "ImportArchive: this datahub mounts no archive import lane, so the `archive_import` capability is \
             absent from its Welcome.features. Nothing was read and nothing was written. Its startup log says why: \
             either it has no project directory above it (so no imports root, <project>/market_data/imports), or it \
             was built without `--features backfill-serve`, which carries the format registry.",
        /// The request failed the shared validator (`crate::archive::validate_import_spec`).
        ImportSpecRefused { why: String } => "datahub.import.spec_refused": "ImportArchive: {why}",
        /// The request named a format this datahub does not register; the format is the client's and is not echoed.
        ImportFormatUnknown { registered: String } => "datahub.import.format.unknown":
            "ImportArchive: this datahub registers no archive format by that name. It registers [{registered}]. \
             Nothing was read and nothing was written.",
        /// The format refused the dataset.
        ImportDatasetRefused { format: &'static str, why: String } => "datahub.import.dataset.refused":
            "ImportArchive {format}: {why} Nothing was read and nothing was written.",
        /// A format that writes a user's file, asked with no target series. It names the CLI's flags, `--lane` the
        /// whole `user-<label>` lane (`--label` is retired, `cli.data.label.retired`).
        ImportTargetMissing { format: &'static str } => "datahub.import.target.missing":
            "ImportArchive {format}: this format writes a user's file, whose series its rows cannot name: give the \
             venue, the symbol, the interval and the lane (--venue, --symbol, --interval, --lane user-<label>). \
             Nothing was read and nothing was written.",
        /// A format that names its own series, asked with a target.
        ImportTargetUnwanted { format: &'static str } => "datahub.import.target.unwanted":
            "ImportArchive {format}: this format names its own series from its files, so it takes no venue, symbol, \
             interval or lane. Nothing was read and nothing was written.",
        /// `replace_holed` asked of a format that replaces no stored day.
        ImportReplacesNoDay { format: &'static str } => "datahub.import.replace_holed.unsupported":
            "ImportArchive {format}: this format replaces no stored day. Replacing a holed day is for a format that \
             reads a vendor's own verified daily files into the venue's series (decision 0268, point 4). Nothing was \
             read and nothing was written.",
        /// The format refused the folder the request reads.
        ImportRequestDirRefused { format: &'static str, why: String } => "datahub.import.request_dir.refused":
            "ImportArchive {format}: {why} Nothing was read and nothing was written.",
        /// Another decoding request holds the one slot: ONE import per datahub, a second refused, not queued.
        ImportBusy => "datahub.import.busy":
            "ImportArchive: another archive import (or `verify`) is already running on this datahub, and it runs ONE \
             at a time — each holds a whole day's decoded ticks in memory, and two would double that inside one \
             memory cap. Nothing was read and nothing was written; send it again once the running one has answered. \
             (A plan-only dry run decodes nothing and is not held to this.)",
        /// The dataset's walk refused.
        ImportWalkRefused { format: &'static str, why: String } => "datahub.import.walk.refused":
            "ImportArchive {format}: {why}",
        /// A probe file could not be placed.
        ImportProbeRefused { format: &'static str, why: String } => "datahub.import.probe.refused":
            "ImportArchive {format}: {why} Nothing was written.",
        /// The window, its omitted bounds filled in, is over the day cap.
        ImportOverDayCap { format: &'static str, days: i64, cap: i64 } => "datahub.import.over_day_cap":
            "ImportArchive {format}: with its omitted bound(s) filled in from the dataset, this request would decode \
             {days} days, over IMPORT_MAX_DAYS = {cap}: one request is synchronous and this wire carries no progress \
             frame. Send explicit bounds and split the window by calendar month (a plan-only dry run is not bounded \
             by this cap). Nothing was read and nothing was written.",
        /// The format would not open its session.
        ImportSessionRefused { format: &'static str, why: String } => "datahub.import.session.refused":
            "ImportArchive {format}: {why}",
        /// The store failed at one day of an import.
        ImportStoreFailed { format: &'static str, dataset: String, day: i64, why: String }
            => "datahub.import.store_failed":
            "ImportArchive {format} {dataset}: the store failed at day {day}: {why}. Every day before it is stored \
             and stays so; repeating the request resumes there.",
        // ---- the archive download (`crates/vike-datahub/src/import/mod.rs`'s `fetch_archive_verb`) ----
        /// A server whose lane mounts no download.
        FetchNoDownload => "datahub.fetch.no_download":
            "FetchArchive: this datahub mounts no archive download, so `archive_fetch` is absent from its \
             Welcome.features. Nothing was listed and nothing was downloaded. Its startup log says why: no import \
             lane (no project above it, or a build without `--features backfill-serve`); the SEC downloads mount on \
             every lane that has one.",
        /// The request failed the shared validator (`crate::archive_fetch::validate_fetch_spec`).
        FetchSpecRefused { why: String } => "datahub.fetch.spec_refused": "FetchArchive: {why}",
        /// A format no mounted download fills.
        FetchFormatNotDownloaded { formats: String } => "datahub.fetch.format.not_downloaded":
            "FetchArchive: this datahub downloads only the [{formats}] archives. Another format's files are put in \
             its folder by hand. Nothing was listed and nothing was downloaded.",
        /// The format refused the dataset.
        FetchDatasetRefused { format: &'static str, why: String } => "datahub.fetch.dataset.refused":
            "FetchArchive {format}: {why} Nothing was listed and nothing was downloaded.",
        /// The download failed (no key, a key the bucket refuses, a listing that failed).
        FetchFailed { format: &'static str, why: String } => "datahub.fetch.failed": "FetchArchive {format}: {why}",
        // ---- the CSV upload (`crates/vike-datahub/src/import/upload.rs`, `crates/vike-datahub/src/server/upload.rs`) ----
        /// No import lane carrying the csv-bars format.
        UploadNoLane => "datahub.csv_upload.no_lane":
            "CsvUpload: this datahub takes no CSV upload: it mounts no import lane carrying the csv-bars format, so \
             `csv_upload` is absent from its Welcome.features. Nothing was read and nothing was written. Its startup \
             log says why: it has no project directory above it, or it was built without `--features \
             backfill-serve`.",
        /// The operator has not armed the upload (`flags.datahub_csv_upload`).
        UploadNotArmed => "datahub.csv_upload.not_armed":
            "CsvUpload: this datahub's operator has not turned CSV uploads on, so `csv_upload` is absent from its \
             Welcome.features. Nothing was read and nothing was written. On the datahub's box: `vike-cli config set \
             flags.datahub_csv_upload true`, then restart it.",
        /// The request failed the shared validator (`crate::upload::validate_upload_spec`).
        UploadSpecRefused { why: String } => "datahub.csv_upload.spec_refused": "CsvUpload: {why}",
        /// The declared size is over the format's cap.
        UploadTooLarge { bytes: u64, cap: u64 } => "datahub.csv_upload.too_large":
            "CsvUpload: the file is {bytes} bytes, over this datahub's cap of {cap} bytes for one file. Nothing was \
             read and nothing was written. Split it into files of fewer days, each under another label.",
        /// The format refused the upload's target or columns.
        UploadFormatRefused { why: String } => "datahub.csv_upload.format_refused": "CsvUpload: {why}",
        /// Another upload or archive import holds the one slot.
        UploadBusy => "datahub.csv_upload.busy":
            "CsvUpload: another upload or archive import is running on this datahub, and it runs ONE at a time: each \
             holds a whole file in memory. Nothing was read and nothing was written; send it again once the running \
             one has answered.",
        /// A chunk ran past the declared size.
        UploadOverrun { declared: u64, total: u64 } => "datahub.csv_upload.overrun":
            "CsvUpload: the file's text runs past the {declared} bytes the upload declared ({total} received). The \
             upload is dropped and nothing was stored; send it again.",
        /// The upload ended short of its declared size.
        UploadShort { got: u64, declared: u64 } => "datahub.csv_upload.short":
            "CsvUpload: the upload ended after {got} of the {declared} bytes it declared. Nothing was stored; send \
             the file again.",
        /// The store failed storing the file (a file the decoder refuses is an answer, `CsvUploadDone::refused`).
        UploadFailed { why: String } => "datahub.csv_upload.failed": "CsvUpload: {why}",
        /// Another request arrived mid-upload.
        UploadBrokenOff => "datahub.csv_upload.broken_off":
            "CsvUpload: a request other than the upload's next chunk or its end arrived, so the upload was dropped \
             and nothing was stored, and that request was not served. Send the upload again, then the request.",
        /// A chunk or an end on a connection with no upload open.
        UploadNotOpen => "datahub.csv_upload.not_open":
            "no CSV upload is open on this connection: it was refused, or it ended, or another request broke it off. \
             Nothing of it was stored; open it again with CsvUpload.",
        // ---- the series delete (`crates/vike-datahub/src/server/delete.rs`'s `delete_series_verb`) ----
        /// A KEY-LESS server serves no delete verb at all.
        DeleteKeyless => "datahub.delete.keyless":
            "this datahub holds no hub keys, so it serves no delete verb at all. A key-less server authenticates \
             nothing — its handshake informs, it does not gate — so `Scope::Write` would be a word nothing \
             enforces, and an irreversible verb behind a word is the state \
             docs/decisions/0025-datahub-remote-posture.md exists to prevent. Register a device on this server's \
             access list (`vike-cli backend mint-missing`), or run the delete on the box with `vike-cli data history \
             rm --store DIR`.",
        /// `produced_by` is not an assertion (`vike_data::store::store_kind::resolve_produced_by`).
        DeleteProducedByRefused { why: String } => "datahub.delete.produced_by_refused": "delete_series: {why}",
        /// A sweep with no provenance assertion.
        DeleteSweepUnasserted { selector: String } => "datahub.delete.sweep_unasserted":
            "refusing a SWEEP with no provenance assertion: {selector} matches more than one series, and deleting by \
             name alone is what `produced_by` exists to replace. Name every dimension, or pass the commit-key prefix \
             the rows carry.",
    }
}

/// A store failure as the datahub answers it: one id per kind, the store's words a field, the text byte for byte
/// the error's own `Display`.
impl From<&HistoryStoreError> for DatahubRefusal {
    fn from(error: &HistoryStoreError) -> Self {
        match error.clone() {
            HistoryStoreError::Query(why) => DatahubRefusal::StoreQuery { why },
            HistoryStoreError::Io(why) => DatahubRefusal::StoreIo { why },
            HistoryStoreError::Unsupported(why) => DatahubRefusal::StoreUnsupported { why },
            HistoryStoreError::SupersedeRefused(why) => {
                DatahubRefusal::StoreSupersedeRefused { why }
            }
        }
    }
}

/// What a person reads for a refusal frame: `error [<id>]: <text>` (`vike_utility::errors::render_refusal`).
#[must_use]
pub fn shown(refusal: &HubRefusal) -> String {
    vike_utility::errors::render_refusal(&refusal.id, &refusal.text)
}

impl From<DatahubRefusal> for HubRefusal {
    fn from(refusal: DatahubRefusal) -> Self {
        HubRefusal::new(refusal.id().word(), &refusal)
    }
}

/// The [`crate::proto::Response::Error`] text a client that does not read the frame is answered for `refusal`: a
/// cap carries the words an older engine classes a cap by (`vike_market_stream::legacy`), every other refusal its
/// text.
#[must_use]
pub fn older_text(refusal: HubRefusal) -> String {
    match DatahubRefusalId::from_word(&refusal.id) {
        Some(DatahubRefusalId::StrategyLaneFull) => {
            vike_market_stream::legacy::older_cap_text(&refusal.text)
        }
        _ => refusal.text,
    }
}

/// The error a refused `StrategyOpen` is, decided by the frame's id: the lane's cap is a
/// [`StreamOpenCapped`] (the stream engine then answers every subscribe with a cap), any other refusal its text.
/// Either reads `error [<id>]: <text>` (`vike_utility::errors::render_refusal`): the id is shown, never decided on.
#[must_use]
pub fn open_refused(refusal: HubRefusal) -> io::Error {
    let shown = vike_utility::errors::render_refusal(&refusal.id, &refusal.text);
    match DatahubRefusalId::from_word(&refusal.id) {
        Some(DatahubRefusalId::StrategyLaneFull) => io::Error::other(StreamOpenCapped(shown)),
        _ => io::Error::other(shown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The lane's cap is a cap by its id**: an open refused by it is a `StreamOpenCapped`, and an older client is
    /// answered words its engine classes as a cap; a frame whose id this build does not know is its text alone.
    #[test]
    fn the_lanes_cap_is_a_cap_by_its_id_for_every_reader() {
        let frame = HubRefusal::from(DatahubRefusal::StrategyLaneFull { cap: 3 });
        assert_eq!(frame.id, "datahub.strategy_lane.full");
        let open = open_refused(frame.clone());
        assert!(open.get_ref().is_some_and(|e| e.is::<StreamOpenCapped>()), "{open}");
        assert!(open.to_string().contains("3 streams"), "{open}");
        assert_eq!(open.to_string(), format!("error [datahub.strategy_lane.full]: {}", frame.text));
        assert!(older_text(frame.clone()).starts_with(&frame.text));
        assert_ne!(older_text(frame.clone()), frame.text, "an older reader is told it is a cap");
        let unknown = HubRefusal::new("datahub.newer", "a cap, not a link");
        assert!(
            open_refused(unknown.clone()).get_ref().is_some_and(|e| !e.is::<StreamOpenCapped>())
        );
        assert_eq!(older_text(unknown), "a cap, not a link");
    }

    /// **A store failure is one id per kind and its text is the error's own words**, so an older client reads what
    /// it always read; a person reads the frame `error [<id>]: <text>`.
    #[test]
    fn a_store_failure_keeps_its_words_under_the_id_of_its_kind() {
        let kinds = [
            (HistoryStoreError::Query("q".into()), "datahub.store.query"),
            (HistoryStoreError::Io("i".into()), "datahub.store.io"),
            (HistoryStoreError::Unsupported("u".into()), "datahub.store.unsupported"),
            (HistoryStoreError::SupersedeRefused("s".into()), "datahub.store.supersede_refused"),
        ];
        for (error, id) in kinds {
            let frame = HubRefusal::from(DatahubRefusal::from(&error));
            assert_eq!((frame.id.as_str(), frame.text.as_str()), (id, error.to_string().as_str()));
            assert_eq!(older_text(frame.clone()), error.to_string());
            assert_eq!(shown(&frame), format!("error [{id}]: {error}"));
        }
    }
}
