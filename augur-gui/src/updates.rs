//! In-app update checking and installation.
//!
//! The check runs on a background thread and never blocks startup or a frame.
//! Applying an update is always an explicit choice: the app offers, the person
//! at the keyboard decides.
//!
//! That is not politeness. `augur-core` writes the running version into every
//! recording sidecar, so which binary produced a dataset is part of the
//! record — an update that happened silently mid-session would leave that
//! provenance quietly wrong. For the same reason [`Updates::busy_reason`]
//! blocks installation while a recording or analysis run is in flight.

use crate::toast::ToastTone;

/// How long a successful check is trusted before the app looks again.
#[cfg(feature = "self-update")]
const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// What the update UI should do next.
#[allow(dead_code)] // `Toast`/`Quit` are unreachable without `self-update`.
pub(crate) enum UpdateAction {
    None,
    Toast(String, ToastTone),
    /// The new build is installed or an installer is running; shut down now.
    Quit,
}

#[cfg(feature = "self-update")]
pub(crate) use enabled::Updates;

#[cfg(not(feature = "self-update"))]
pub(crate) use disabled::Updates;

/// Stub used when the crate is built without `self-update`, so packagers who
/// manage upgrades themselves can compile the updater out entirely without
/// `app.rs` needing to know.
#[cfg(not(feature = "self-update"))]
mod disabled {
    use super::UpdateAction;

    #[derive(Default)]
    pub(crate) struct Updates;

    impl Updates {
        pub(crate) fn new() -> Self {
            Self
        }
        pub(crate) fn start_automatic_check(&mut self) {}
        pub(crate) fn poll(&mut self, _ctx: &egui::Context) -> UpdateAction {
            UpdateAction::None
        }
        pub(crate) fn menu(&mut self, ui: &mut egui::Ui) {
            ui.add_enabled(false, egui::Button::new("Check for updates…"))
                .on_disabled_hover_text("This build was compiled without the in-app updater.");
        }
        pub(crate) fn window(&mut self, _ctx: &egui::Context, _busy: Option<&str>) -> UpdateAction {
            UpdateAction::None
        }
    }
}

#[cfg(feature = "self-update")]
mod enabled {
    use super::{UpdateAction, CHECK_INTERVAL_SECS};
    use crate::toast::ToastTone;

    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use augur_update::{Download, Release, UpdateError, UpdateStatus, Version};
    use crossbeam_channel::{bounded, Receiver};

    const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

    enum Phase {
        Idle,
        Checking,
        UpToDate(Version),
        Available(Box<Release>),
        Downloading {
            release: Box<Release>,
            received: Arc<AtomicU64>,
            total: u64,
        },
        Installing,
        Done,
        Failed(String),
    }

    enum Message {
        Checked(Result<UpdateStatus, UpdateError>),
        Downloaded(Result<Download, UpdateError>),
    }

    pub(crate) struct Updates {
        prefs: Prefs,
        phase: Phase,
        inbox: Option<Receiver<Message>>,
        /// Set when the check ran on its own rather than being asked for, so a
        /// background "you are up to date" does not steal focus.
        automatic: bool,
        pub(crate) window_open: bool,
    }

    impl Updates {
        pub(crate) fn new() -> Self {
            Self {
                prefs: Prefs::load(),
                phase: Phase::Idle,
                inbox: None,
                automatic: false,
                window_open: false,
            }
        }

        /// Start the once-a-day check, if it is due and enabled.
        pub(crate) fn start_automatic_check(&mut self) {
            if !self.prefs.check_on_startup || self.prefs.checked_recently() {
                return;
            }
            self.automatic = true;
            self.spawn_check();
        }

        /// Start a check the user explicitly asked for, ignoring the interval.
        pub(crate) fn check_now(&mut self) {
            self.automatic = false;
            self.window_open = true;
            self.spawn_check();
        }

        fn spawn_check(&mut self) {
            if matches!(self.phase, Phase::Checking | Phase::Downloading { .. }) {
                return;
            }
            self.phase = Phase::Checking;

            let (tx, rx) = bounded(1);
            self.inbox = Some(rx);
            std::thread::spawn(move || {
                let _ = tx.send(Message::Checked(augur_update::check(CURRENT_VERSION)));
            });
        }

        fn spawn_download(&mut self, release: Box<Release>) {
            let received = Arc::new(AtomicU64::new(0));
            let total = release.asset.size;

            let (tx, rx) = bounded(1);
            self.inbox = Some(rx);

            let counter = Arc::clone(&received);
            let payload = release.clone();
            std::thread::spawn(move || {
                let result = augur_update::download(&payload, |done, _| {
                    counter.store(done, Ordering::Relaxed);
                });
                let _ = tx.send(Message::Downloaded(result));
            });

            self.phase = Phase::Downloading {
                release,
                received,
                total,
            };
        }

        /// Drain the background thread. Call once per frame.
        pub(crate) fn poll(&mut self, ctx: &egui::Context) -> UpdateAction {
            // A download in flight needs repaints to animate the progress bar;
            // the GUI is otherwise happy to idle.
            if matches!(self.phase, Phase::Checking | Phase::Downloading { .. }) {
                ctx.request_repaint_after(std::time::Duration::from_millis(120));
            }

            let Some(inbox) = &self.inbox else {
                return UpdateAction::None;
            };
            let Ok(message) = inbox.try_recv() else {
                return UpdateAction::None;
            };
            self.inbox = None;

            match message {
                Message::Checked(Ok(UpdateStatus::UpToDate(version))) => {
                    self.prefs.mark_checked();
                    self.phase = Phase::UpToDate(version.clone());
                    if self.automatic {
                        UpdateAction::None
                    } else {
                        UpdateAction::Toast(
                            format!("AugurRS {version} is the latest release."),
                            ToastTone::Success,
                        )
                    }
                }
                Message::Checked(Ok(UpdateStatus::Available(release))) => {
                    self.prefs.mark_checked();
                    let version = release.version.clone();
                    let skipped = self.prefs.skipped.as_deref() == Some(&version.to_string());
                    self.phase = Phase::Available(release);

                    if self.automatic && skipped {
                        UpdateAction::None
                    } else {
                        self.window_open = true;
                        UpdateAction::Toast(
                            format!("AugurRS {version} is available."),
                            ToastTone::Info,
                        )
                    }
                }
                Message::Checked(Err(error)) => {
                    let message = error.to_string();
                    self.phase = Phase::Failed(message.clone());
                    // A failed background check is not the user's problem: the
                    // machine may simply be offline in a lab.
                    if self.automatic {
                        UpdateAction::None
                    } else {
                        UpdateAction::Toast(message, ToastTone::Warn)
                    }
                }
                Message::Downloaded(Ok(download)) => {
                    self.phase = Phase::Installing;
                    match augur_update::apply(&download) {
                        Ok(_) => {
                            self.phase = Phase::Done;
                            UpdateAction::Quit
                        }
                        Err(error) => {
                            augur_update::discard(&download);
                            let message = error.to_string();
                            self.phase = Phase::Failed(message.clone());
                            UpdateAction::Toast(message, ToastTone::Error)
                        }
                    }
                }
                Message::Downloaded(Err(error)) => {
                    let message = error.to_string();
                    self.phase = Phase::Failed(message.clone());
                    UpdateAction::Toast(message, ToastTone::Error)
                }
            }
        }

        pub(crate) fn menu(&mut self, ui: &mut egui::Ui) {
            let busy = matches!(self.phase, Phase::Checking | Phase::Downloading { .. });
            let label = if busy {
                "Checking for updates…"
            } else {
                "Check for updates…"
            };
            if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
                self.check_now();
                ui.close_menu();
            }

            let mut auto = self.prefs.check_on_startup;
            if ui
                .checkbox(&mut auto, "Check on startup")
                .on_hover_text(
                    "Looks for a newer release at most once a day. Updates are never installed \
                     without asking.",
                )
                .changed()
            {
                self.prefs.check_on_startup = auto;
                self.prefs.save();
            }

            if ui.button("Release notes").clicked() {
                ui.ctx()
                    .open_url(egui::OpenUrl::new_tab(augur_update::releases_url()));
                ui.close_menu();
            }
        }

        pub(crate) fn window(&mut self, ctx: &egui::Context, busy: Option<&str>) -> UpdateAction {
            if !self.window_open {
                return UpdateAction::None;
            }

            let mut action = UpdateAction::None;
            let mut open = self.window_open;

            egui::Window::new("Software update")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(420.0)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    action = self.window_body(ui, busy);
                });

            self.window_open = open;
            action
        }

        fn window_body(&mut self, ui: &mut egui::Ui, busy: Option<&str>) -> UpdateAction {
            ui.label(format!("Installed version: {CURRENT_VERSION}"));
            ui.separator();

            let mut start_download = None;
            let mut action = UpdateAction::None;

            match &self.phase {
                Phase::Idle => {
                    ui.label("No check has run yet.");
                    if ui.button("Check now").clicked() {
                        self.check_now();
                    }
                }
                Phase::Checking => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Contacting the release server…");
                    });
                }
                Phase::UpToDate(version) => {
                    ui.label(format!("AugurRS {version} is the latest release."));
                }
                Phase::Available(release) => {
                    ui.heading(format!("AugurRS {} is available", release.version));
                    if !release.published_at.is_empty() {
                        ui.weak(format!("Published {}", release.published_at));
                    }
                    ui.add_space(4.0);

                    if !release.notes.is_empty() {
                        egui::ScrollArea::vertical()
                            .max_height(160.0)
                            .show(ui, |ui| {
                                ui.label(release.notes.trim());
                            });
                        ui.add_space(4.0);
                    }
                    if ui.link("Open the full release notes").clicked() {
                        ui.ctx()
                            .open_url(egui::OpenUrl::new_tab(release.notes_url.clone()));
                    }
                    ui.add_space(8.0);

                    match install_blocker(busy) {
                        Some(reason) => {
                            ui.colored_label(ui.visuals().warn_fg_color, reason);
                        }
                        None => {
                            ui.horizontal(|ui| {
                                if ui.button("Download and install").clicked() {
                                    start_download = Some(release.clone());
                                }
                                if ui.button("Skip this version").clicked() {
                                    self.prefs.skipped = Some(release.version.to_string());
                                    self.prefs.save();
                                    self.window_open = false;
                                }
                            });
                            ui.add_space(4.0);
                            ui.weak(
                                "The download is verified against the release checksums before \
                                 anything is replaced. AugurRS restarts when it is done.",
                            );
                        }
                    }
                }
                Phase::Downloading {
                    release,
                    received,
                    total,
                } => {
                    let done = received.load(Ordering::Relaxed);
                    ui.label(format!("Downloading AugurRS {}…", release.version));
                    let bar = if *total > 0 {
                        egui::ProgressBar::new(done as f32 / *total as f32).text(format!(
                            "{} / {}",
                            mib(done),
                            mib(*total)
                        ))
                    } else {
                        egui::ProgressBar::new(0.0).animate(true).text(mib(done))
                    };
                    ui.add(bar);
                }
                Phase::Installing => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Installing…");
                    });
                }
                Phase::Done => {
                    ui.label("Update installed. AugurRS is restarting.");
                }
                Phase::Failed(message) => {
                    ui.colored_label(ui.visuals().error_fg_color, message);
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Try again").clicked() {
                            self.check_now();
                        }
                        if ui.button("Open the releases page").clicked() {
                            ui.ctx()
                                .open_url(egui::OpenUrl::new_tab(augur_update::releases_url()));
                        }
                    });
                }
            }

            if let Some(release) = start_download {
                self.spawn_download(release);
                action = UpdateAction::None;
            }

            action
        }
    }

    /// Reasons an update must not start right now.
    fn install_blocker(busy: Option<&str>) -> Option<String> {
        if let Some(what) = busy {
            return Some(format!(
                "{what} is still running. AugurRS stamps its version into every recording, so \
                 finish or stop it before updating."
            ));
        }

        // Ask before downloading rather than after: there is no point pulling
        // 15 MB only to discover this copy cannot be replaced.
        match augur_update::install_kind() {
            Ok(_) => None,
            Err(error) => Some(format!(
                "{error}. Download the new release manually instead."
            )),
        }
    }

    fn mib(bytes: u64) -> String {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }

    /// Update preferences, kept in the platform config directory.
    ///
    /// Deliberately not part of `CameraConfig`: that struct is serialised into
    /// recording sidecars, and how often this machine phones home for updates
    /// is not experiment metadata.
    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct Prefs {
        #[serde(default = "default_true")]
        check_on_startup: bool,
        #[serde(default)]
        last_check_unix: u64,
        #[serde(default)]
        skipped: Option<String>,
    }

    fn default_true() -> bool {
        true
    }

    impl Default for Prefs {
        fn default() -> Self {
            Self {
                check_on_startup: true,
                last_check_unix: 0,
                skipped: None,
            }
        }
    }

    impl Prefs {
        fn path() -> Option<std::path::PathBuf> {
            Some(dirs::config_dir()?.join("augur").join("updates.json"))
        }

        fn load() -> Self {
            let Some(path) = Self::path() else {
                return Self::default();
            };
            std::fs::read_to_string(path)
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                // A corrupt or unreadable prefs file must not stop the app from
                // starting; defaults are always safe here.
                .unwrap_or_default()
        }

        fn save(&self) {
            let Some(path) = Self::path() else {
                return;
            };
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(text) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(path, text);
            }
        }

        fn checked_recently(&self) -> bool {
            now_unix().saturating_sub(self.last_check_unix) < CHECK_INTERVAL_SECS
        }

        fn mark_checked(&mut self) {
            self.last_check_unix = now_unix();
            self.save();
        }
    }

    fn now_unix() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or_default()
    }
}
