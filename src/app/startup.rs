//! Startup recovery renders while native attachment and profile preparation run off-thread.
use super::*;
use crate::control::frontend::{Client, Recovery};
use std::sync::mpsc::TryRecvError;

#[cfg(test)]
mod tests;

pub(super) struct ProfileResources {
    pub blocklist: Blocklist,
    pub behaviors: Result<BehaviorCatalog, String>,
}

impl ProfileResources {
    pub fn load() -> Self {
        Self {
            blocklist: Blocklist::load_cache().unwrap_or_default(),
            behaviors: BehaviorCatalog::load(),
        }
    }
}

struct Prepared {
    client: Client,
    settings: ClientSettings,
    resources: ProfileResources,
    access_failed: bool,
}

pub(super) struct Startup {
    recovery: Recovery,
    paths: VecDeque<PathBuf>,
    preparing: Option<Receiver<Prepared>>,
    worker_error: Option<&'static str>,
}

impl Startup {
    pub fn new(directory: PathBuf, paths: Vec<PathBuf>) -> Self {
        let mut startup = Self {
            recovery: Recovery::new(directory),
            paths: paths.into(),
            preparing: None,
            worker_error: None,
        };
        startup.retry();
        startup
    }

    fn retry(&mut self) {
        self.worker_error = self
            .recovery
            .retry()
            .err()
            .map(|_| "Could not start the background connection worker.");
    }

    pub fn poll(&mut self, ctx: &egui::Context) -> Option<PeerLumeApp> {
        if let Some(client) = self.recovery.poll() {
            let (sender, receiver) = mpsc::sync_channel(1);
            let context = ctx.clone();
            match std::thread::Builder::new()
                .name("peerlume-prepare-gui".into())
                .spawn(move || {
                    let settings = client.settings();
                    let root = Path::new(&settings.download.download_directory);
                    let access_failed = client.configure_agents(root).is_err();
                    let _ = client.set_agent_settings(&settings);
                    let resources = ProfileResources::load();
                    let _ = sender.send(Prepared {
                        client,
                        settings,
                        resources,
                        access_failed,
                    });
                    context.request_repaint();
                }) {
                Ok(_) => self.preparing = Some(receiver),
                Err(_) => {
                    self.worker_error = Some("Could not start the desktop preparation worker.")
                }
            }
        }
        match self.preparing.as_ref()?.try_recv() {
            Ok(prepared) => {
                self.preparing = None;
                let mut app = PeerLumeApp::from_backend(
                    ClientBackend::from_attached(prepared.client),
                    prepared.settings,
                    prepared.resources,
                );
                app.pending_torrent_paths = std::mem::take(&mut self.paths);
                app.open_next_queued_torrent();
                if prepared.access_failed {
                    app.desktop_error("Trusted agent access", None, "Could not initialize the local access panel. Transfers still belong to the background client.".into());
                }
                Some(app)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.preparing = None;
                self.worker_error =
                    Some("The desktop preparation worker stopped before reporting a result.");
                None
            }
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        let busy = self.recovery.busy() || self.preparing.is_some();
        let message = if self.preparing.is_some() {
            "Background client connected. Preparing the desktop..."
        } else {
            self.worker_error.unwrap_or_else(|| self.recovery.message())
        };
        let mut action = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            action = recovery_panel(ui, self.recovery.directory(), message, busy);
        });
        match action {
            Some(Action::Retry) => self.retry(),
            Some(Action::Validate) => {
                self.worker_error = self
                    .recovery
                    .validate()
                    .err()
                    .map(|_| "Could not start the configuration validation worker.");
            }
            Some(Action::CopyProfile) => {
                ctx.copy_text(self.recovery.directory().to_string_lossy().into_owned());
            }
            None => {}
        }
        if busy || self.recovery.busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Retry,
    Validate,
    CopyProfile,
}

fn recovery_panel(
    ui: &mut egui::Ui,
    directory: &Path,
    message: &str,
    busy: bool,
) -> Option<Action> {
    ui.heading("Background client startup");
    if busy {
        ui.spinner();
    }
    ui.add(egui::Label::new(message).wrap());
    ui.strong("Profile location");
    ui.add(egui::Label::new(directory.to_string_lossy()).wrap());
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(!busy, egui::Button::new("Retry connection"))
            .clicked()
        {
            action = Some(Action::Retry);
        }
        if ui
            .add_enabled(!busy, egui::Button::new("Validate configuration"))
            .clicked()
        {
            action = Some(Action::Validate);
        }
        if ui.button("Copy profile location").clicked() {
            action = Some(Action::CopyProfile);
        }
    });
    action
}
