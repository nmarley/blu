//! Shared CLI progress UI primitives and backup progress event protocol.
//!
//! Domain code emits [`BackupEvent`] values through a [`BackupProgress`] sink.
//! Only a UI consumer task (or the null sink) should touch `indicatif`.
//!
//! Helpers here are `pub(crate)` for the backup orchestrator. Allow dead_code
//! so the protocol can land ahead of the command wiring without false alarms.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::time::Duration;

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use tokio::sync::mpsc;

use crate::format::human_bytes;

/// Default capacity for the backup event channel.
pub(crate) const BACKUP_EVENT_CHANNEL_CAPACITY: usize = 256;

/// Default number of concurrent active-work rows under the phase line.
pub(crate) const DEFAULT_ACTIVE_SLOTS: usize = 4;

/// Steady-tick interval so spinners never look wedged during quiet I/O.
const STEADY_TICK: Duration = Duration::from_millis(100);

/// Whether progress bars should be drawn for this process.
///
/// Returns false when `quiet` is set or when stderr is not a terminal.
pub(crate) fn should_draw_progress(quiet: bool) -> bool {
    !quiet && io::stderr().is_terminal()
}

/// Shared style for the overall backup bar (percent + message holds human sizes).
pub(crate) fn bar_style() -> ProgressStyle {
    ProgressStyle::default_bar()
        .template("{prefix:.bold} {bar:40.cyan/blue} {percent:>3}% [{elapsed_precise}] {msg}")
        .expect("valid progress bar template")
        .progress_chars("=>-")
}

/// Countable bar style used by restore/mirror-style single bars.
#[allow(dead_code)] // reserved for shared callers beyond backup
pub(crate) fn count_bar_style() -> ProgressStyle {
    ProgressStyle::default_bar()
        .template("{bar:40} {pos}/{len} [{elapsed_precise}] {msg}")
        .expect("valid progress bar template")
}

/// Style for the phase line (spinner with message).
pub(crate) fn phase_style() -> ProgressStyle {
    ProgressStyle::default_spinner()
        .template("{prefix:.bold} {spinner:.green} {msg}")
        .expect("valid phase spinner template")
}

/// Style for capped active-work rows.
pub(crate) fn active_style() -> ProgressStyle {
    ProgressStyle::default_spinner()
        .template("  {spinner:.dim} {prefix:<6} {msg}")
        .expect("valid active row template")
}

/// High-level backup phase for the phase line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackupPhase {
    /// Walking and hashing source files.
    Index,
    /// Packing missing chunks and uploading sealed blobs.
    EncryptUpload,
    /// Waiting for in-flight uploads to finish.
    Finalize,
    /// Pushing catalog indexes to the backend.
    PushIndexes,
}

impl BackupPhase {
    /// Short label for the phase line.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::EncryptUpload => "encrypt+upload",
            Self::Finalize => "finalize",
            Self::PushIndexes => "push indexes",
        }
    }
}

/// Telemetry-only backup progress event.
///
/// No ciphertext, keys, or business payloads. Safe to drop non-critical
/// ticks when the UI channel is full; failures still surface via join errors.
#[derive(Debug, Clone)]
pub(crate) enum BackupEvent {
    /// Enter a named phase.
    Phase(BackupPhase),
    /// Planned index work after pre-scan.
    IndexPlan {
        /// Number of files to hash.
        files: u64,
        /// Total bytes to hash.
        bytes: u64,
    },
    /// Started hashing one file.
    IndexFileStarted {
        /// Path being hashed (display only).
        path: PathBuf,
        /// File size in bytes.
        bytes: u64,
    },
    /// Additional bytes hashed within the current file.
    IndexFileProgress {
        /// Bytes hashed since the last tick.
        bytes_delta: u64,
    },
    /// Finished hashing the current file.
    IndexFileFinished,
    /// Planned encrypt/upload work for missing chunks.
    EncryptPlan {
        /// Number of missing chunks.
        chunks: u64,
        /// Plaintext bytes of missing chunks.
        bytes: u64,
    },
    /// A chunk was appended into the blob buffer (counts toward overall).
    ChunkSealed {
        /// Plaintext chunk size in bytes.
        bytes: u64,
    },
    /// A blob was sealed and handed to a background put.
    BlobSealed {
        /// Short content id for the active row.
        short_id: String,
        /// Encrypted blob size in bytes.
        bytes: u64,
    },
    /// A background put completed successfully.
    BlobUploaded {
        /// Short content id for the active row.
        short_id: String,
        /// Encrypted blob size in bytes.
        bytes: u64,
    },
    /// A background put failed (UI notice; join path still errors).
    BlobUploadFailed {
        /// Short content id for the active row.
        short_id: String,
        /// Display error message.
        error: String,
    },
    /// Catalog push finished.
    PushIndexesFinished,
}

/// Sink for backup telemetry events.
pub(crate) trait BackupProgress: Send + Sync {
    /// Emit one progress event.
    fn emit(&self, event: BackupEvent);
}

/// No-op sink for tests, quiet mode, and library callers.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct NullProgress;

impl BackupProgress for NullProgress {
    fn emit(&self, _event: BackupEvent) {}
}

/// Channel-backed sink. Prefer [`Self::try_emit`]-style delivery via `emit`.
///
/// Uses `try_send` so async producers never deadlock against the UI task on
/// the same runtime. Non-critical ticks may be dropped if the channel is full.
pub(crate) struct ChannelProgress {
    tx: mpsc::Sender<BackupEvent>,
}

impl ChannelProgress {
    /// Wrap an existing sender.
    pub(crate) fn new(tx: mpsc::Sender<BackupEvent>) -> Self {
        Self { tx }
    }

    /// Build a bounded channel and return `(sink, receiver)`.
    pub(crate) fn channel() -> (Self, mpsc::Receiver<BackupEvent>) {
        let (tx, rx) = mpsc::channel(BACKUP_EVENT_CHANNEL_CAPACITY);
        (Self::new(tx), rx)
    }
}

impl BackupProgress for ChannelProgress {
    fn emit(&self, event: BackupEvent) {
        let _ = self.tx.try_send(event);
    }
}

impl BackupProgress for std::sync::Arc<dyn BackupProgress> {
    fn emit(&self, event: BackupEvent) {
        (**self).emit(event);
    }
}

/// One active-work row under the phase line.
struct ActiveSlot {
    id: String,
    bar: ProgressBar,
}

/// Capped pool of active-work rows. Oldest rows are evicted when full.
pub(crate) struct ActiveSlotPool {
    multi: MultiProgress,
    capacity: usize,
    order: VecDeque<String>,
    slots: Vec<ActiveSlot>,
    style: ProgressStyle,
    enabled: bool,
}

impl ActiveSlotPool {
    /// Create a pool attached to `multi` with a hard row cap.
    pub(crate) fn new(multi: MultiProgress, capacity: usize, enabled: bool) -> Self {
        Self {
            multi,
            capacity: capacity.max(1),
            order: VecDeque::new(),
            slots: Vec::new(),
            style: active_style(),
            enabled,
        }
    }

    /// Insert or update a row identified by `id`.
    pub(crate) fn upsert(&mut self, id: impl Into<String>, kind: &str, message: impl AsRef<str>) {
        if !self.enabled || self.capacity == 0 {
            return;
        }
        let id = id.into();
        let message = message.as_ref();

        if let Some(slot) = self.slots.iter().find(|s| s.id == id) {
            slot.bar.set_prefix(kind.to_string());
            slot.bar.set_message(message.to_string());
            slot.bar.tick();
            return;
        }

        while self.slots.len() >= self.capacity {
            self.evict_oldest();
        }

        let bar = self.multi.add(ProgressBar::new_spinner());
        bar.set_style(self.style.clone());
        bar.set_prefix(kind.to_string());
        bar.set_message(message.to_string());
        bar.enable_steady_tick(STEADY_TICK);
        self.order.push_back(id.clone());
        self.slots.push(ActiveSlot { id, bar });
    }

    /// Remove the row for `id` if present.
    pub(crate) fn remove(&mut self, id: &str) {
        if !self.enabled {
            return;
        }
        if let Some(pos) = self.slots.iter().position(|s| s.id == id) {
            let slot = self.slots.remove(pos);
            slot.bar.finish_and_clear();
            self.multi.remove(&slot.bar);
            self.order.retain(|x| x != id);
        }
    }

    /// Drop every active row.
    pub(crate) fn clear(&mut self) {
        if !self.enabled {
            return;
        }
        for slot in self.slots.drain(..) {
            slot.bar.finish_and_clear();
            self.multi.remove(&slot.bar);
        }
        self.order.clear();
    }

    /// Number of currently displayed rows (test helper).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether `id` currently occupies a row (test helper).
    #[cfg(test)]
    pub(crate) fn contains(&self, id: &str) -> bool {
        self.slots.iter().any(|s| s.id == id)
    }

    fn evict_oldest(&mut self) {
        let Some(oldest) = self.order.pop_front() else {
            return;
        };
        if let Some(pos) = self.slots.iter().position(|s| s.id == oldest) {
            let slot = self.slots.remove(pos);
            slot.bar.finish_and_clear();
            self.multi.remove(&slot.bar);
        }
    }
}

/// Multi-bar backup progress display owned by the UI consumer task.
pub(crate) struct ProgressUi {
    multi: MultiProgress,
    overall: ProgressBar,
    phase: ProgressBar,
    active: ActiveSlotPool,
    enabled: bool,
}

impl ProgressUi {
    /// Build a UI. When drawing is disabled, bars use a hidden draw target.
    pub(crate) fn new(quiet: bool) -> Self {
        let enabled = should_draw_progress(quiet);
        let multi = if enabled {
            MultiProgress::new()
        } else {
            MultiProgress::with_draw_target(ProgressDrawTarget::hidden())
        };

        let overall = multi.add(ProgressBar::new(0));
        overall.set_style(bar_style());
        overall.set_prefix("Backup");
        overall.set_message("starting");
        if enabled {
            overall.enable_steady_tick(STEADY_TICK);
        }

        let phase = multi.add(ProgressBar::new_spinner());
        phase.set_style(phase_style());
        phase.set_prefix("Phase");
        phase.set_message("starting");
        if enabled {
            phase.enable_steady_tick(STEADY_TICK);
        }

        let active = ActiveSlotPool::new(multi.clone(), DEFAULT_ACTIVE_SLOTS, enabled);

        Self {
            multi,
            overall,
            phase,
            active,
            enabled,
        }
    }

    /// Whether this UI will paint to the terminal.
    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Set the overall byte denominator (0 is allowed and shows an unbounded bar).
    pub(crate) fn set_total_bytes(&self, total: u64) {
        if total == 0 {
            self.overall.set_length(1);
            self.overall.set_position(0);
        } else {
            self.overall.set_length(total);
        }
    }

    /// Replace the overall completed-byte counter.
    pub(crate) fn set_position_bytes(&self, pos: u64) {
        self.overall.set_position(pos);
    }

    /// Advance the overall completed-byte counter.
    pub(crate) fn inc_bytes(&self, delta: u64) {
        if delta > 0 {
            self.overall.inc(delta);
        }
    }

    /// Update the overall trailing message (rates, human totals, etc.).
    pub(crate) fn set_overall_message(&self, msg: impl Into<String>) {
        self.overall.set_message(msg.into());
    }

    /// Update the phase line.
    pub(crate) fn set_phase(&self, phase: BackupPhase, detail: impl AsRef<str>) {
        let detail = detail.as_ref();
        if detail.is_empty() {
            self.phase.set_message(phase.label().to_string());
        } else {
            self.phase
                .set_message(format!("{}  {}", phase.label(), detail));
        }
    }

    /// Insert or refresh an active-work row.
    pub(crate) fn active_upsert(
        &mut self,
        id: impl Into<String>,
        kind: &str,
        message: impl AsRef<str>,
    ) {
        self.active.upsert(id, kind, message);
    }

    /// Remove an active-work row.
    pub(crate) fn active_remove(&mut self, id: &str) {
        self.active.remove(id);
    }

    /// Clear all active-work rows.
    pub(crate) fn active_clear(&mut self) {
        self.active.clear();
    }

    /// Run `f` with drawing suspended (for eprintln of errors).
    pub(crate) fn suspend<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        self.multi.suspend(f)
    }

    /// Finish and clear all bars.
    pub(crate) fn finish(mut self) {
        self.active.clear();
        self.phase.finish_and_clear();
        self.overall.finish_and_clear();
        let _ = self.multi.clear();
    }
}

/// Running counters for the backup UI consumer.
#[derive(Debug, Default, Clone)]
pub(crate) struct BackupUiState {
    /// Current phase.
    pub phase: Option<BackupPhase>,
    /// Planned hash bytes (index phase contribution to overall).
    pub index_bytes_total: u64,
    /// Hashed bytes so far.
    pub index_bytes_done: u64,
    /// Planned files to hash.
    pub index_files_total: u64,
    /// Files finished hashing.
    pub index_files_done: u64,
    /// Planned missing-chunk plaintext bytes.
    pub encrypt_bytes_total: u64,
    /// Plaintext chunk bytes sealed into buffers so far.
    pub encrypt_bytes_done: u64,
    /// Planned missing chunks.
    pub encrypt_chunks_total: u64,
    /// Chunks sealed so far.
    pub encrypt_chunks_done: u64,
    /// Blobs sealed (handed to background put).
    pub blobs_sealed: u64,
    /// Blobs whose put completed successfully.
    pub blobs_uploaded: u64,
    /// Blobs whose put failed.
    pub blobs_failed: u64,
    /// Puts still in flight (sealed - uploaded - failed, floored at 0).
    pub uploads_in_flight: u64,
    /// Encrypted bytes uploaded successfully.
    pub upload_bytes_done: u64,
    /// Active hash-row id for the file currently being indexed.
    current_index_id: Option<String>,
}

impl BackupUiState {
    /// Overall work denominator: hash bytes + missing-chunk plain bytes.
    pub(crate) fn overall_total(&self) -> u64 {
        self.index_bytes_total
            .saturating_add(self.encrypt_bytes_total)
    }

    /// Overall work completed toward [`Self::overall_total`].
    pub(crate) fn overall_done(&self) -> u64 {
        self.index_bytes_done
            .saturating_add(self.encrypt_bytes_done)
    }

    fn refresh_in_flight(&mut self) {
        let finished = self.blobs_uploaded.saturating_add(self.blobs_failed);
        self.uploads_in_flight = self.blobs_sealed.saturating_sub(finished);
    }
}

/// Apply one backup event to the UI and running state.
pub(crate) fn apply_backup_event(
    ui: &mut ProgressUi,
    state: &mut BackupUiState,
    event: BackupEvent,
) {
    match event {
        BackupEvent::Phase(phase) => {
            state.phase = Some(phase);
            ui.set_phase(phase, phase_detail(state, phase));
        }
        BackupEvent::IndexPlan { files, bytes } => {
            state.index_files_total = files;
            state.index_bytes_total = bytes;
            ui.set_total_bytes(state.overall_total());
            ui.set_position_bytes(state.overall_done());
            if state.phase.is_none() {
                state.phase = Some(BackupPhase::Index);
            }
            ui.set_phase(
                state.phase.unwrap_or(BackupPhase::Index),
                phase_detail(state, BackupPhase::Index),
            );
            refresh_overall_message(ui, state);
        }
        BackupEvent::IndexFileStarted { path, bytes } => {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let id = format!("hash:{}", path.display());
            if let Some(prev) = state.current_index_id.take() {
                ui.active_remove(&prev);
            }
            state.current_index_id = Some(id.clone());
            ui.active_upsert(id, "hash", format!("{}  {}", name, human_bytes(bytes)));
        }
        BackupEvent::IndexFileProgress { bytes_delta } => {
            state.index_bytes_done = state.index_bytes_done.saturating_add(bytes_delta);
            ui.set_position_bytes(state.overall_done());
            refresh_overall_message(ui, state);
            if let Some(phase) = state.phase {
                ui.set_phase(phase, phase_detail(state, phase));
            }
        }
        BackupEvent::IndexFileFinished => {
            state.index_files_done = state.index_files_done.saturating_add(1);
            if let Some(id) = state.current_index_id.take() {
                ui.active_remove(&id);
            }
            if let Some(phase) = state.phase {
                ui.set_phase(phase, phase_detail(state, phase));
            }
            refresh_overall_message(ui, state);
        }
        BackupEvent::EncryptPlan { chunks, bytes } => {
            state.encrypt_chunks_total = chunks;
            state.encrypt_bytes_total = bytes;
            ui.set_total_bytes(state.overall_total());
            ui.set_position_bytes(state.overall_done());
            if state.phase.is_none() {
                state.phase = Some(BackupPhase::EncryptUpload);
            }
            ui.active_clear();
            ui.set_phase(
                state.phase.unwrap_or(BackupPhase::EncryptUpload),
                phase_detail(state, BackupPhase::EncryptUpload),
            );
            refresh_overall_message(ui, state);
        }
        BackupEvent::ChunkSealed { bytes } => {
            state.encrypt_bytes_done = state.encrypt_bytes_done.saturating_add(bytes);
            state.encrypt_chunks_done = state.encrypt_chunks_done.saturating_add(1);
            ui.set_position_bytes(state.overall_done());
            if let Some(phase) = state.phase {
                ui.set_phase(phase, phase_detail(state, phase));
            }
            refresh_overall_message(ui, state);
        }
        BackupEvent::BlobSealed { short_id, bytes } => {
            state.blobs_sealed = state.blobs_sealed.saturating_add(1);
            state.refresh_in_flight();
            let id = format!("blob:{short_id}");
            ui.active_upsert(id, "put", format!("{short_id}  {}", human_bytes(bytes)));
            if let Some(phase) = state.phase {
                ui.set_phase(phase, phase_detail(state, phase));
            }
            refresh_overall_message(ui, state);
        }
        BackupEvent::BlobUploaded { short_id, bytes } => {
            state.blobs_uploaded = state.blobs_uploaded.saturating_add(1);
            state.upload_bytes_done = state.upload_bytes_done.saturating_add(bytes);
            state.refresh_in_flight();
            ui.active_remove(&format!("blob:{short_id}"));
            if let Some(phase) = state.phase {
                ui.set_phase(phase, phase_detail(state, phase));
            }
            refresh_overall_message(ui, state);
        }
        BackupEvent::BlobUploadFailed { short_id, error } => {
            state.blobs_failed = state.blobs_failed.saturating_add(1);
            state.refresh_in_flight();
            ui.active_remove(&format!("blob:{short_id}"));
            ui.suspend(|| {
                eprintln!("  upload failed ({short_id}): {error}");
            });
            if let Some(phase) = state.phase {
                ui.set_phase(phase, phase_detail(state, phase));
            }
            refresh_overall_message(ui, state);
        }
        BackupEvent::PushIndexesFinished => {
            ui.active_clear();
            ui.set_phase(BackupPhase::PushIndexes, "done");
            refresh_overall_message(ui, state);
        }
    }
}

fn phase_detail(state: &BackupUiState, phase: BackupPhase) -> String {
    match phase {
        BackupPhase::Index => format!(
            "{}/{} files  {}/{}",
            state.index_files_done,
            state.index_files_total,
            human_bytes(state.index_bytes_done),
            human_bytes(state.index_bytes_total),
        ),
        BackupPhase::EncryptUpload => format!(
            "{}/{} chunks  {}/{} blobs  {} in flight",
            state.encrypt_chunks_done,
            state.encrypt_chunks_total,
            state.blobs_uploaded,
            state.blobs_sealed,
            state.uploads_in_flight,
        ),
        BackupPhase::Finalize => format!(
            "{} upload(s) remaining  {}",
            state.uploads_in_flight,
            human_bytes(state.upload_bytes_done),
        ),
        BackupPhase::PushIndexes => "syncing catalog".to_string(),
    }
}

fn refresh_overall_message(ui: &ProgressUi, state: &BackupUiState) {
    ui.set_overall_message(format!(
        "{}/{}  uploaded {}",
        human_bytes(state.overall_done()),
        human_bytes(state.overall_total()),
        human_bytes(state.upload_bytes_done),
    ));
}

/// Drain a receiver, applying every event to a quiet/hidden UI (test helper path).
#[cfg(test)]
async fn drain_events_quiet(mut rx: mpsc::Receiver<BackupEvent>) -> BackupUiState {
    let mut ui = ProgressUi::new(true);
    let mut state = BackupUiState::default();
    while let Some(event) = rx.recv().await {
        apply_backup_event(&mut ui, &mut state, event);
    }
    ui.finish();
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_progress_accepts_all_events() {
        let sink = NullProgress;
        sink.emit(BackupEvent::Phase(BackupPhase::Index));
        sink.emit(BackupEvent::IndexPlan {
            files: 3,
            bytes: 1024,
        });
        sink.emit(BackupEvent::IndexFileStarted {
            path: PathBuf::from("a.bin"),
            bytes: 10,
        });
        sink.emit(BackupEvent::IndexFileProgress { bytes_delta: 5 });
        sink.emit(BackupEvent::IndexFileFinished);
        sink.emit(BackupEvent::EncryptPlan {
            chunks: 2,
            bytes: 20,
        });
        sink.emit(BackupEvent::ChunkSealed { bytes: 10 });
        sink.emit(BackupEvent::BlobSealed {
            short_id: "abc".into(),
            bytes: 64,
        });
        sink.emit(BackupEvent::BlobUploaded {
            short_id: "abc".into(),
            bytes: 64,
        });
        sink.emit(BackupEvent::BlobUploadFailed {
            short_id: "def".into(),
            error: "nope".into(),
        });
        sink.emit(BackupEvent::PushIndexesFinished);
    }

    #[test]
    fn channel_progress_delivers_events_in_order() {
        let (sink, mut rx) = ChannelProgress::channel();
        sink.emit(BackupEvent::Phase(BackupPhase::Index));
        sink.emit(BackupEvent::IndexPlan { files: 1, bytes: 9 });
        sink.emit(BackupEvent::Phase(BackupPhase::EncryptUpload));

        let mut phases = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let BackupEvent::Phase(p) = ev {
                phases.push(p);
            }
        }
        assert_eq!(phases, vec![BackupPhase::Index, BackupPhase::EncryptUpload]);
    }

    #[test]
    fn active_slot_pool_caps_and_evicts_oldest() {
        let multi = MultiProgress::with_draw_target(ProgressDrawTarget::hidden());
        let mut pool = ActiveSlotPool::new(multi, 2, true);

        pool.upsert("a", "hash", "one");
        pool.upsert("b", "hash", "two");
        assert_eq!(pool.len(), 2);
        assert!(pool.contains("a"));
        assert!(pool.contains("b"));

        pool.upsert("c", "put", "three");
        assert_eq!(pool.len(), 2);
        assert!(!pool.contains("a"));
        assert!(pool.contains("b"));
        assert!(pool.contains("c"));

        pool.upsert("b", "put", "two-updated");
        assert_eq!(pool.len(), 2);
        assert!(pool.contains("b"));
        assert!(pool.contains("c"));

        pool.remove("b");
        assert_eq!(pool.len(), 1);
        assert!(!pool.contains("b"));
        assert!(pool.contains("c"));

        pool.clear();
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn progress_ui_hidden_when_quiet() {
        let ui = ProgressUi::new(true);
        assert!(!ui.is_enabled());
        ui.set_total_bytes(100);
        ui.inc_bytes(40);
        ui.set_phase(BackupPhase::Index, "scanning");
        ui.finish();
    }

    #[test]
    fn backup_phase_labels_are_stable() {
        assert_eq!(BackupPhase::Index.label(), "index");
        assert_eq!(BackupPhase::EncryptUpload.label(), "encrypt+upload");
        assert_eq!(BackupPhase::Finalize.label(), "finalize");
        assert_eq!(BackupPhase::PushIndexes.label(), "push indexes");
    }

    #[test]
    fn apply_backup_event_tracks_overall_bytes() {
        let mut ui = ProgressUi::new(true);
        let mut state = BackupUiState::default();

        apply_backup_event(
            &mut ui,
            &mut state,
            BackupEvent::IndexPlan {
                files: 2,
                bytes: 100,
            },
        );
        apply_backup_event(
            &mut ui,
            &mut state,
            BackupEvent::IndexFileStarted {
                path: PathBuf::from("a.bin"),
                bytes: 40,
            },
        );
        apply_backup_event(
            &mut ui,
            &mut state,
            BackupEvent::IndexFileProgress { bytes_delta: 40 },
        );
        apply_backup_event(&mut ui, &mut state, BackupEvent::IndexFileFinished);
        apply_backup_event(
            &mut ui,
            &mut state,
            BackupEvent::EncryptPlan {
                chunks: 3,
                bytes: 60,
            },
        );
        apply_backup_event(&mut ui, &mut state, BackupEvent::ChunkSealed { bytes: 20 });
        apply_backup_event(&mut ui, &mut state, BackupEvent::ChunkSealed { bytes: 40 });
        apply_backup_event(
            &mut ui,
            &mut state,
            BackupEvent::BlobSealed {
                short_id: "abc1234".into(),
                bytes: 50,
            },
        );
        apply_backup_event(
            &mut ui,
            &mut state,
            BackupEvent::BlobUploaded {
                short_id: "abc1234".into(),
                bytes: 50,
            },
        );

        assert_eq!(state.overall_total(), 160);
        assert_eq!(state.overall_done(), 100);
        assert_eq!(state.index_files_done, 1);
        assert_eq!(state.encrypt_chunks_done, 2);
        assert_eq!(state.blobs_sealed, 1);
        assert_eq!(state.blobs_uploaded, 1);
        assert_eq!(state.uploads_in_flight, 0);
        assert_eq!(state.upload_bytes_done, 50);
        ui.finish();
    }

    #[tokio::test]
    async fn channel_drain_updates_state() {
        let (sink, rx) = ChannelProgress::channel();
        sink.emit(BackupEvent::Phase(BackupPhase::Index));
        sink.emit(BackupEvent::IndexPlan { files: 1, bytes: 8 });
        sink.emit(BackupEvent::IndexFileProgress { bytes_delta: 8 });
        sink.emit(BackupEvent::Phase(BackupPhase::EncryptUpload));
        sink.emit(BackupEvent::EncryptPlan {
            chunks: 1,
            bytes: 8,
        });
        sink.emit(BackupEvent::ChunkSealed { bytes: 8 });
        drop(sink);

        let state = drain_events_quiet(rx).await;
        assert_eq!(state.overall_total(), 16);
        assert_eq!(state.overall_done(), 16);
        assert_eq!(state.phase, Some(BackupPhase::EncryptUpload));
    }
}
