//! Analysis runs: deterministic, range-based plugin analysis as the primary
//! analysis workflow. A run captures its full configuration (input file,
//! time range, window, plugin selection + settings), executes on a background
//! thread through `run_offline_analysis`, and keeps its results available in
//! the session so the workspace can always say which run produced them.
//!
//! Live plugin output remains available as a preview; runs are the exact,
//! exportable results. See `docs/features/analysis-runs.md` and ADR 025.

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
};

use augur_runtime::{
    probe_replay_file, run_offline_analysis, LivePluginHostSnapshot, OfflineAnalysisConfig,
    OfflineAnalysisOptions, OfflineAnalysisSummary, OfflineProgress, ReplayFileProbe,
};

use crate::theme;

/// One plugin the user can include in a run, seeded from the GUI plugin
/// manager when the dialog opens.
#[derive(Debug, Clone)]
pub(crate) struct PluginChoice {
    pub name: String,
    pub phase: &'static str,
    pub selected: bool,
}

/// Everything the app needs to start a run. The app turns this into an
/// `OfflineAnalysisConfig` by snapshotting the selected plugins' current
/// settings from the GUI plugin manager.
#[derive(Debug, Clone)]
pub(crate) struct AnalysisStartRequest {
    pub name: String,
    pub input_path: PathBuf,
    pub output_dir: PathBuf,
    pub t_start_us: Option<u64>,
    pub t_end_us: Option<u64>,
    pub acq_time_us: u64,
    pub selected_plugins: Vec<String>,
}

pub(crate) enum AnalysisRunState {
    Running {
        rx: mpsc::Receiver<Result<OfflineAnalysisSummary, String>>,
        progress_rx: mpsc::Receiver<OfflineProgress>,
        stop: Arc<AtomicBool>,
        latest_progress: Option<OfflineProgress>,
        cancel_requested: bool,
    },
    Completed {
        processed_windows: u64,
        exported_files: usize,
        host_snapshots: Vec<LivePluginHostSnapshot>,
    },
    Failed(String),
    Cancelled,
}

pub(crate) struct AnalysisRun {
    pub id: u64,
    pub name: String,
    pub input_path: PathBuf,
    pub output_dir: PathBuf,
    pub range_label: String,
    pub window_label: String,
    pub plugin_names: Vec<String>,
    pub state: AnalysisRunState,
}

impl AnalysisRun {
    pub fn is_running(&self) -> bool {
        matches!(self.state, AnalysisRunState::Running { .. })
    }
}

/// Lifecycle notifications the app turns into toasts / result publication.
pub(crate) enum AnalysisRunEvent {
    Finished { id: u64 },
    Failed { id: u64, error: String },
}

/// Actions the runs panel asks the app to perform.
pub(crate) enum AnalysisPanelAction {
    NewAnalysis,
    ViewResults(u64),
    OpenFolder(u64),
    Remove(u64),
}

enum TimelineState {
    Missing,
    Probing(mpsc::Receiver<Result<ReplayFileProbe, String>>),
    Ready { first_us: u64, last_us: u64 },
    Failed(String),
}

impl AnalysisDialog {
    pub fn input_path(&self) -> Option<&Path> {
        self.input_path.as_deref()
    }
}

pub(crate) struct AnalysisDialog {
    pub open: bool,
    input_path: Option<PathBuf>,
    timeline: TimelineState,
    use_custom_range: bool,
    start_seconds: f64,
    end_seconds: f64,
    window_ms: u64,
    run_name: String,
    output_parent: String,
    plugins: Vec<PluginChoice>,
    error: Option<String>,
}

/// Per-frame context the dialog needs from the app.
pub(crate) struct AnalysisDialogContext {
    /// Displayed-frame end timestamp, absolute µs, when a replay of the
    /// dialog's input file is open. Enables "from playhead" quick-set.
    pub playhead_us: Option<u64>,
    /// Another run is currently executing (one at a time).
    pub run_active: bool,
}

pub(crate) struct AnalysisRunsState {
    pub panel_open: bool,
    pub dialog: AnalysisDialog,
    runs: Vec<AnalysisRun>,
    next_run_id: u64,
}

impl Default for AnalysisRunsState {
    fn default() -> Self {
        Self {
            panel_open: false,
            dialog: AnalysisDialog {
                open: false,
                input_path: None,
                timeline: TimelineState::Missing,
                use_custom_range: false,
                start_seconds: 0.0,
                end_seconds: 0.0,
                window_ms: 1,
                run_name: String::new(),
                output_parent: String::new(),
                plugins: Vec::new(),
                error: None,
            },
            runs: Vec::new(),
            next_run_id: 1,
        }
    }
}

impl AnalysisRunsState {
    pub fn runs(&self) -> &[AnalysisRun] {
        &self.runs
    }

    pub fn run(&self, id: u64) -> Option<&AnalysisRun> {
        self.runs.iter().find(|run| run.id == id)
    }

    pub fn any_running(&self) -> bool {
        self.runs.iter().any(AnalysisRun::is_running)
    }

    /// Open the configuration dialog. `timeline` is `(first_ts, last_ts)` in
    /// absolute µs when the input file is already open as a replay; otherwise
    /// the dialog probes the file in the background.
    pub fn open_dialog(
        &mut self,
        input_path: Option<PathBuf>,
        timeline: Option<(u64, u64)>,
        window_ms: u64,
        plugins: Vec<PluginChoice>,
    ) {
        let dialog = &mut self.dialog;
        dialog.open = true;
        dialog.error = None;
        dialog.window_ms = window_ms.max(1);
        dialog.plugins = plugins;
        dialog.use_custom_range = false;
        dialog.start_seconds = 0.0;
        dialog.input_path = None;
        dialog.timeline = TimelineState::Missing;
        if let Some(path) = input_path {
            dialog.set_input(path, timeline, self.next_run_id);
        } else {
            dialog.run_name = format!("Run {}", self.next_run_id);
            dialog.output_parent = String::new();
        }
    }

    /// Spawn the background thread for a validated request. The app has
    /// already turned the request into a full plugin configuration.
    pub fn start_run(
        &mut self,
        request: &AnalysisStartRequest,
        config: OfflineAnalysisConfig,
    ) -> u64 {
        let id = self.next_run_id;
        self.next_run_id += 1;

        let (tx, rx) = mpsc::channel();
        let (progress_tx, progress_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let input_path = request.input_path.clone();
        let output_dir = request.output_dir.clone();
        let session_id = request.name.clone();
        thread::spawn(move || {
            let result = run_offline_analysis(
                OfflineAnalysisOptions {
                    input_path,
                    output_dir,
                    plugins_dir: None,
                    config,
                    stop: Some(worker_stop),
                    session_id: Some(session_id),
                },
                |progress| {
                    let _ = progress_tx.send(progress);
                },
            );
            let _ = tx.send(result);
        });

        self.runs.insert(
            0,
            AnalysisRun {
                id,
                name: request.name.clone(),
                input_path: request.input_path.clone(),
                output_dir: request.output_dir.clone(),
                range_label: range_label(request.t_start_us, request.t_end_us),
                window_label: window_label(request.acq_time_us),
                plugin_names: request.selected_plugins.clone(),
                state: AnalysisRunState::Running {
                    rx,
                    progress_rx,
                    stop,
                    latest_progress: None,
                    cancel_requested: false,
                },
            },
        );
        self.panel_open = true;
        id
    }

    /// Drain progress and completion messages from running runs.
    pub fn poll(&mut self) -> Vec<AnalysisRunEvent> {
        let mut events = Vec::new();
        for run in &mut self.runs {
            let AnalysisRunState::Running {
                rx,
                progress_rx,
                latest_progress,
                cancel_requested,
                ..
            } = &mut run.state
            else {
                continue;
            };
            while let Ok(progress) = progress_rx.try_recv() {
                *latest_progress = Some(progress);
            }
            let cancelled = *cancel_requested;
            let outcome = match rx.try_recv() {
                Ok(outcome) => outcome,
                Err(mpsc::TryRecvError::Empty) => continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err("analysis task ended unexpectedly".to_owned())
                }
            };
            run.state = match outcome {
                Ok(summary) => {
                    events.push(AnalysisRunEvent::Finished { id: run.id });
                    AnalysisRunState::Completed {
                        processed_windows: summary.processed_windows,
                        exported_files: summary.exported_files.len(),
                        host_snapshots: summary.host_snapshots,
                    }
                }
                Err(_) if cancelled => AnalysisRunState::Cancelled,
                Err(error) => {
                    events.push(AnalysisRunEvent::Failed {
                        id: run.id,
                        error: error.clone(),
                    });
                    AnalysisRunState::Failed(error)
                }
            };
        }
        events
    }

    pub fn request_cancel(&mut self, id: u64) {
        if let Some(run) = self.runs.iter_mut().find(|run| run.id == id) {
            if let AnalysisRunState::Running {
                stop,
                cancel_requested,
                ..
            } = &mut run.state
            {
                stop.store(true, Ordering::Relaxed);
                *cancel_requested = true;
            }
        }
    }

    pub fn remove_run(&mut self, id: u64) {
        self.runs
            .retain(|run| run.id != id || matches!(run.state, AnalysisRunState::Running { .. }));
    }

    /// Draw the Analysis Runs window. Returns actions for the app to apply.
    pub fn show_panel(
        &mut self,
        ctx: &egui::Context,
        viewed_run: Option<u64>,
    ) -> Vec<AnalysisPanelAction> {
        if !self.panel_open {
            return Vec::new();
        }
        let mut actions = Vec::new();
        let mut cancel_requested = None;
        let mut open = self.panel_open;
        egui::Window::new("Analysis Runs")
            .open(&mut open)
            .default_width(430.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(
                            "Deterministic analysis over a recording — exact, exportable results.",
                        )
                        .weak(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(theme::primary_button("New Analysis…")).clicked() {
                            actions.push(AnalysisPanelAction::NewAnalysis);
                        }
                    });
                });
                ui.separator();

                if self.runs.is_empty() {
                    ui.add_space(theme::sp::SP_4);
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new("No analysis runs yet.").weak());
                        ui.small("Start one to compute exact plugin results over a file or range.");
                    });
                    ui.add_space(theme::sp::SP_4);
                    return;
                }

                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for run in &self.runs {
                            draw_run_card(
                                ui,
                                run,
                                viewed_run == Some(run.id),
                                &mut actions,
                                &mut cancel_requested,
                            );
                            ui.add_space(theme::sp::SP_2);
                        }
                    });
                ui.small(
                    "Live plugin output in the workspace is a preview; \
                     runs hold the exact results.",
                );
            });
        self.panel_open = open;
        if let Some(id) = cancel_requested {
            self.request_cancel(id);
        }
        actions
    }

    /// Draw the run-configuration dialog. Returns a validated start request
    /// when the user hits Start.
    pub fn show_dialog(
        &mut self,
        ctx: &egui::Context,
        dctx: &AnalysisDialogContext,
    ) -> Option<AnalysisStartRequest> {
        if !self.dialog.open {
            return None;
        }
        self.dialog.poll_probe();

        let next_run_id = self.next_run_id;
        let mut request = None;
        let mut open = self.dialog.open;
        egui::Window::new("New Analysis")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(430.0)
            .show(ctx, |ui| {
                request = self.dialog.draw_contents(ui, dctx, next_run_id);
            });
        self.dialog.open = open && request.is_none();
        request
    }
}

impl AnalysisDialog {
    fn set_input(&mut self, path: PathBuf, timeline: Option<(u64, u64)>, next_run_id: u64) {
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("recording")
            .to_owned();
        self.run_name = format!("Run {next_run_id} — {stem}");
        self.output_parent = path
            .parent()
            .map(|parent| parent.display().to_string())
            .unwrap_or_default();
        self.timeline = match timeline {
            Some((first_us, last_us)) => TimelineState::Ready { first_us, last_us },
            None => {
                let (tx, rx) = mpsc::channel();
                let probe_path = path.clone();
                thread::spawn(move || {
                    let _ = tx.send(probe_replay_file(&probe_path));
                });
                TimelineState::Probing(rx)
            }
        };
        self.input_path = Some(path);
        self.error = None;
        self.reset_range_to_timeline();
    }

    fn poll_probe(&mut self) {
        if let TimelineState::Probing(rx) = &self.timeline {
            match rx.try_recv() {
                Ok(Ok(probe)) => {
                    self.timeline = TimelineState::Ready {
                        first_us: probe.first_event_ts_us,
                        last_us: probe.last_event_ts_us,
                    };
                    self.reset_range_to_timeline();
                }
                Ok(Err(err)) => self.timeline = TimelineState::Failed(err),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.timeline = TimelineState::Failed("file probe ended unexpectedly".into());
                }
            }
        }
    }

    fn duration_seconds(&self) -> Option<f64> {
        match self.timeline {
            TimelineState::Ready { first_us, last_us } => {
                Some((last_us.saturating_sub(first_us)) as f64 / 1_000_000.0)
            }
            _ => None,
        }
    }

    fn reset_range_to_timeline(&mut self) {
        self.start_seconds = 0.0;
        self.end_seconds = self.duration_seconds().unwrap_or(0.0);
    }

    fn draw_contents(
        &mut self,
        ui: &mut egui::Ui,
        dctx: &AnalysisDialogContext,
        next_run_id: u64,
    ) -> Option<AnalysisStartRequest> {
        ui.label(
            "Runs every selected plugin over the chosen range with fixed \
             windows. Results are deterministic and exported to disk.",
        );
        ui.small("Selected plugins run with their current settings.");
        ui.separator();

        // ── Input file ────────────────────────────────────────────────
        ui.horizontal(|ui| {
            theme::field_label(ui, "Input", None);
            let label = self
                .input_path
                .as_ref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "no file selected".to_owned());
            ui.label(egui::RichText::new(label).strong()).on_hover_text(
                self.input_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Browse…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Replay Files", &["raw", "csv", "bin", "npy", "h5", "hdf5"])
                        .pick_file()
                    {
                        self.set_input(path, None, next_run_id);
                    }
                }
            });
        });
        match &self.timeline {
            TimelineState::Probing(_) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.small("Reading file timeline…");
                });
            }
            TimelineState::Failed(err) => {
                ui.colored_label(ui.visuals().error_fg_color, format!("Probe failed: {err}"));
            }
            TimelineState::Ready { first_us, last_us } => {
                let span_us = last_us.saturating_sub(*first_us);
                ui.small(format!(
                    "Events from {} to {} ({})",
                    format_seconds(0.0),
                    format_seconds(span_us as f64 / 1_000_000.0),
                    format_us_span(span_us),
                ));
            }
            TimelineState::Missing => {}
        }
        ui.separator();

        // ── Range ─────────────────────────────────────────────────────
        let timeline_ready = matches!(self.timeline, TimelineState::Ready { .. });
        ui.horizontal(|ui| {
            theme::field_label(ui, "Range", None);
            ui.radio_value(&mut self.use_custom_range, false, "Whole file");
            ui.add_enabled_ui(timeline_ready, |ui| {
                ui.radio_value(&mut self.use_custom_range, true, "Time range");
            });
        });
        if self.use_custom_range && timeline_ready {
            let duration = self.duration_seconds().unwrap_or(0.0).max(0.001);
            self.start_seconds = self.start_seconds.clamp(0.0, duration);
            self.end_seconds = self.end_seconds.clamp(self.start_seconds, duration);
            let start_changed = ui
                .add(
                    egui::Slider::new(&mut self.start_seconds, 0.0..=duration)
                        .text("Start [s]")
                        .fixed_decimals(3),
                )
                .changed();
            if start_changed {
                self.end_seconds = self.end_seconds.max(self.start_seconds);
            }
            ui.add(
                egui::Slider::new(&mut self.end_seconds, self.start_seconds..=duration)
                    .text("End [s]")
                    .fixed_decimals(3),
            );
            if let Some(playhead_us) = dctx.playhead_us {
                if let TimelineState::Ready { first_us, .. } = self.timeline {
                    let playhead_s =
                        (playhead_us.saturating_sub(first_us) as f64 / 1_000_000.0).min(duration);
                    ui.horizontal(|ui| {
                        ui.small(format!("Playhead: {}", format_seconds(playhead_s)));
                        if ui.small_button("Start from playhead").clicked() {
                            self.start_seconds = playhead_s;
                            self.end_seconds = self.end_seconds.max(playhead_s);
                        }
                        if ui.small_button("End at playhead").clicked() {
                            self.end_seconds = playhead_s;
                            self.start_seconds = self.start_seconds.min(playhead_s);
                        }
                    });
                }
            }
        }
        ui.separator();

        // ── Window ────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            theme::field_label(ui, "Window", Some("ms"));
            ui.add(egui::DragValue::new(&mut self.window_ms).range(1..=60_000));
            ui.small("events are grouped into fixed windows of this length");
        });
        if let Some(windows) = self.estimated_windows() {
            ui.small(format!("Estimated windows: {windows}"));
        }
        ui.separator();

        // ── Plugins ───────────────────────────────────────────────────
        theme::section_subhead(ui, "Plugins");
        if self.plugins.is_empty() {
            ui.small("No plugins loaded — open the Plugin Manager to install plugins.");
        } else {
            for choice in &mut self.plugins {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut choice.selected, &choice.name);
                    theme::chip(ui, choice.phase, theme::Tone::Neutral);
                });
            }
        }
        ui.separator();

        // ── Name and output ───────────────────────────────────────────
        ui.horizontal(|ui| {
            theme::field_label(ui, "Name", None);
            ui.add(egui::TextEdit::singleline(&mut self.run_name).desired_width(280.0));
        });
        ui.horizontal(|ui| {
            theme::field_label(ui, "Output", None);
            ui.add(egui::TextEdit::singleline(&mut self.output_parent).desired_width(240.0));
            if ui.button("Browse…").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.output_parent = path.display().to_string();
                }
            }
        });
        ui.small(format!("Will create: {}", self.output_dir_name()));

        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if dctx.run_active {
            ui.small("Another analysis is still running — one run executes at a time.");
        }

        ui.separator();
        let mut request = None;
        ui.horizontal(|ui| {
            let can_start = !dctx.run_active && self.input_path.is_some();
            if ui
                .add_enabled(can_start, theme::primary_button("Start analysis"))
                .clicked()
            {
                match self.build_request() {
                    Ok(built) => request = Some(built),
                    Err(err) => self.error = Some(err),
                }
            }
            if ui.button("Cancel").clicked() {
                self.open = false;
            }
        });
        request
    }

    fn estimated_windows(&self) -> Option<u64> {
        let duration = self.duration_seconds()?;
        let (start, end) = if self.use_custom_range {
            (self.start_seconds, self.end_seconds)
        } else {
            (0.0, duration)
        };
        let span_us = ((end - start).max(0.0) * 1_000_000.0).round() as u64;
        let window_us = self.window_ms.max(1).saturating_mul(1_000);
        Some(span_us.div_ceil(window_us).max(1))
    }

    fn output_dir_name(&self) -> String {
        let parent = self.output_parent.trim();
        let name = slugify(&self.run_name);
        if parent.is_empty() {
            name
        } else {
            Path::new(parent).join(name).display().to_string()
        }
    }

    fn build_request(&self) -> Result<AnalysisStartRequest, String> {
        let input_path = self
            .input_path
            .clone()
            .ok_or_else(|| "choose an input file".to_owned())?;
        if matches!(self.timeline, TimelineState::Probing(_)) {
            return Err("still reading the file timeline — wait a moment".into());
        }
        let run_name = self.run_name.trim();
        if run_name.is_empty() {
            return Err("give the run a name".into());
        }
        if self.output_parent.trim().is_empty() {
            return Err("choose an output folder".into());
        }
        let selected_plugins: Vec<String> = self
            .plugins
            .iter()
            .filter(|choice| choice.selected)
            .map(|choice| choice.name.clone())
            .collect();
        if selected_plugins.is_empty() {
            return Err("select at least one plugin".into());
        }

        let (t_start_us, t_end_us) = if self.use_custom_range {
            let TimelineState::Ready { first_us, last_us } = self.timeline else {
                return Err("time range needs a readable file timeline".into());
            };
            if self.end_seconds <= self.start_seconds {
                return Err("end time must be greater than start time".into());
            }
            let duration = self.duration_seconds().unwrap_or(0.0);
            let start_us = first_us.saturating_add(seconds_to_us(self.start_seconds));
            // The slider maxed out means "through the end of the file":
            // extend past the last event so the half-open range includes it.
            let end_us = if self.end_seconds >= duration {
                last_us.saturating_add(1)
            } else {
                first_us.saturating_add(seconds_to_us(self.end_seconds))
            };
            (Some(start_us), Some(end_us))
        } else {
            (None, None)
        };

        let output_dir = PathBuf::from(self.output_dir_name());
        if output_dir.exists() {
            return Err(format!(
                "output folder {} already exists — rename the run or pick another folder",
                output_dir.display()
            ));
        }

        Ok(AnalysisStartRequest {
            name: run_name.to_owned(),
            input_path,
            output_dir,
            t_start_us,
            t_end_us,
            acq_time_us: self.window_ms.max(1).saturating_mul(1_000),
            selected_plugins,
        })
    }
}

fn draw_run_card(
    ui: &mut egui::Ui,
    run: &AnalysisRun,
    is_viewed: bool,
    actions: &mut Vec<AnalysisPanelAction>,
    cancel_requested: &mut Option<u64>,
) {
    theme::card_frame(ui).show(ui, |ui| {
        ui.horizontal(|ui| {
            match &run.state {
                AnalysisRunState::Running { .. } => {
                    ui.spinner();
                }
                AnalysisRunState::Completed { .. } => {
                    ui.label(
                        egui::RichText::new(egui_phosphor::regular::CHECK_CIRCLE)
                            .color(theme::palette_for_visuals(ui.visuals()).status_success),
                    );
                }
                AnalysisRunState::Failed(_) => {
                    ui.label(
                        egui::RichText::new(egui_phosphor::regular::WARNING_OCTAGON)
                            .color(ui.visuals().error_fg_color),
                    );
                }
                AnalysisRunState::Cancelled => {
                    ui.label(
                        egui::RichText::new(egui_phosphor::regular::X_CIRCLE)
                            .color(theme::palette_for_visuals(ui.visuals()).fg_3),
                    );
                }
            }
            ui.label(egui::RichText::new(&run.name).strong());
            if is_viewed {
                theme::chip(ui, "SHOWN", theme::Tone::Info);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !run.is_running()
                    && theme::icon_button(ui, egui_phosphor::regular::TRASH, "Remove from list")
                        .clicked()
                {
                    actions.push(AnalysisPanelAction::Remove(run.id));
                }
            });
        });

        let input_name = run
            .input_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| run.input_path.display().to_string());
        ui.small(format!(
            "{input_name} \u{00B7} {} \u{00B7} {} \u{00B7} {} plugin(s)",
            run.range_label,
            run.window_label,
            run.plugin_names.len()
        ))
        .on_hover_text(run.plugin_names.join(", "));

        match &run.state {
            AnalysisRunState::Running {
                latest_progress, ..
            } => {
                let (fraction, text) = match latest_progress {
                    Some(progress) => {
                        let total = progress.total_windows.max(1);
                        (
                            progress.processed_windows as f32 / total as f32,
                            format!("{}/{} windows", progress.processed_windows, total),
                        )
                    }
                    None => (0.0, "starting…".to_owned()),
                };
                ui.horizontal(|ui| {
                    ui.add(
                        egui::ProgressBar::new(fraction)
                            .desired_width(ui.available_width() - 80.0)
                            .text(text),
                    );
                    if ui.button("Cancel").clicked() {
                        *cancel_requested = Some(run.id);
                    }
                });
            }
            AnalysisRunState::Completed {
                processed_windows,
                exported_files,
                ..
            } => {
                ui.horizontal(|ui| {
                    if ui.button("View results").clicked() {
                        actions.push(AnalysisPanelAction::ViewResults(run.id));
                    }
                    if ui.button("Open folder").clicked() {
                        actions.push(AnalysisPanelAction::OpenFolder(run.id));
                    }
                    ui.small(format!(
                        "{processed_windows} window(s), {exported_files} file(s)"
                    ));
                });
            }
            AnalysisRunState::Failed(error) => {
                ui.colored_label(ui.visuals().error_fg_color, truncate(error, 120))
                    .on_hover_text(error);
            }
            AnalysisRunState::Cancelled => {
                ui.small("Cancelled.");
            }
        }
    });
}

fn range_label(t_start_us: Option<u64>, t_end_us: Option<u64>) -> String {
    match (t_start_us, t_end_us) {
        (None, None) => "whole file".to_owned(),
        (start, end) => format!(
            "[{}, {}) us",
            start.map_or_else(|| "start".to_owned(), |us| us.to_string()),
            end.map_or_else(|| "end".to_owned(), |us| us.to_string()),
        ),
    }
}

fn window_label(acq_time_us: u64) -> String {
    if acq_time_us.is_multiple_of(1_000) {
        format!("{} ms windows", acq_time_us / 1_000)
    } else {
        format!("{acq_time_us} us windows")
    }
}

fn format_seconds(seconds: f64) -> String {
    format!("{seconds:.3} s")
}

fn format_us_span(span_us: u64) -> String {
    if span_us >= 1_000_000 {
        format!("{:.3} s", span_us as f64 / 1_000_000.0)
    } else if span_us >= 1_000 {
        format!("{:.1} ms", span_us as f64 / 1_000.0)
    } else {
        format!("{span_us} us")
    }
}

fn seconds_to_us(seconds: f64) -> u64 {
    (seconds.max(0.0) * 1_000_000.0).round() as u64
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(max_chars).collect();
        format!("{cut}…")
    }
}

fn slugify(value: &str) -> String {
    let slug: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = slug.trim_matches('_');
    if trimmed.is_empty() {
        "analysis_run".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_label_covers_whole_file_and_custom_ranges() {
        assert_eq!(range_label(None, None), "whole file");
        assert_eq!(range_label(Some(10), Some(20)), "[10, 20) us");
        assert_eq!(range_label(None, Some(20)), "[start, 20) us");
    }

    #[test]
    fn slugify_produces_filesystem_safe_names() {
        assert_eq!(slugify("Run 3 — sample.raw"), "run_3___sample_raw");
        assert_eq!(slugify("???"), "analysis_run");
    }

    #[test]
    fn custom_range_end_at_slider_max_includes_last_event() {
        let dialog = AnalysisDialog {
            open: true,
            input_path: Some(PathBuf::from("/tmp/example.csv")),
            timeline: TimelineState::Ready {
                first_us: 100,
                last_us: 1_100,
            },
            use_custom_range: true,
            start_seconds: 0.0,
            end_seconds: 0.0011,
            window_ms: 1,
            run_name: "augur test run xyzzy range".into(),
            output_parent: std::env::temp_dir().display().to_string(),
            plugins: vec![PluginChoice {
                name: "demo".into(),
                phase: "Frame",
                selected: true,
            }],
            error: None,
        };
        let request = dialog.build_request().expect("request");
        // end slider at file duration → half-open end extends past the last
        // event so it is included
        assert_eq!(request.t_end_us, Some(1_101));
        assert_eq!(request.t_start_us, Some(100));
    }

    #[test]
    fn build_request_rejects_missing_plugin_selection() {
        let dialog = AnalysisDialog {
            open: true,
            input_path: Some(PathBuf::from("/tmp/example.csv")),
            timeline: TimelineState::Ready {
                first_us: 0,
                last_us: 1_000,
            },
            use_custom_range: false,
            start_seconds: 0.0,
            end_seconds: 0.001,
            window_ms: 1,
            run_name: "augur test run xyzzy plugins".into(),
            output_parent: std::env::temp_dir().display().to_string(),
            plugins: Vec::new(),
            error: None,
        };
        let err = dialog.build_request().expect_err("no plugins selected");
        assert!(err.contains("plugin"));
    }
}
