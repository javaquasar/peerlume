//! Inspect a user-selected portable build without executing its files.
mod package;

use eframe::egui;
use package::VerifiedPackage;
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, TryRecvError},
};

type Inspection = Result<Option<VerifiedPackage>, String>;

#[derive(Default)]
pub(super) struct UpdatesPanel {
    open: bool,
    pending: Option<Receiver<Inspection>>,
    result: Option<Result<VerifiedPackage, String>>,
}

impl UpdatesPanel {
    pub(super) fn open(&mut self) {
        self.open = true;
    }

    pub(super) fn show(&mut self, ctx: &egui::Context) {
        self.poll();
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new("Build and updates")
            .open(&mut open)
            .default_width(480.0)
            .max_width((ctx.content_rect().width() - 24.0).max(120.0))
            .vscroll(true)
            .show(ctx, |ui| self.contents(ui));
        self.open = open;
    }

    fn contents(&mut self, ui: &mut egui::Ui) {
        ui.heading(format!("PeerLume {}", env!("CARGO_PKG_VERSION")));
        ui.label(format!(
            "Current native protocol {}",
            crate::control::frontend::PROTOCOL_VERSION
        ));
        ui.separator();
        ui.label("Inspect a downloaded desktop build. Extract the archive before choosing its package folder.");
        if ui
            .add_enabled(
                self.pending.is_none(),
                egui::Button::new("Choose package folder…"),
            )
            .clicked()
        {
            self.start(ui.ctx(), || {
                rfd::FileDialog::new()
                    .set_title("Choose the extracted PeerLume desktop package")
                    .pick_folder()
            });
        }
        if self.pending.is_some() {
            ui.horizontal_wrapped(|ui| {
                ui.spinner();
                ui.label("Waiting for folder selection or checking package files…");
            });
        }
        match self.result.as_ref() {
            Some(Ok(candidate)) => show_candidate(ui, candidate),
            Some(Err(message)) => {
                ui.label(egui::RichText::new("Package verification failed").strong());
                ui.add(egui::Label::new(message).wrap());
            }
            None => {}
        }
    }

    fn start(
        &mut self,
        ctx: &egui::Context,
        pick: impl FnOnce() -> Option<PathBuf> + Send + 'static,
    ) {
        if self.pending.is_some() {
            return;
        }
        self.result = None;
        let (sender, receiver) = mpsc::sync_channel(1);
        let ctx = ctx.clone();
        match std::thread::Builder::new()
            .name("package-preview".into())
            .spawn(move || {
                let result = pick().map(|folder| package::verify(&folder)).transpose();
                let _ = sender.send(result);
                ctx.request_repaint();
            }) {
            Ok(_) => self.pending = Some(receiver),
            Err(error) => {
                self.result = Some(Err(format!("Cannot start package verification: {error}")))
            }
        }
    }

    fn poll(&mut self) {
        let Some(receiver) = &self.pending else {
            return;
        };
        self.result = match receiver.try_recv() {
            Ok(result) => result.transpose(),
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                Some(Err("Package verification worker ended unexpectedly".into()))
            }
        };
        self.pending = None;
    }
}

fn show_candidate(ui: &mut egui::Ui, package: &VerifiedPackage) {
    ui.separator();
    ui.heading(format!("Desktop package {}", package.version));
    ui.label(format!("{} · {}", package.platform, package.target));
    ui.add(egui::Label::new(format!("Manifest source commit {}", package.commit)).wrap());
    ui.add(egui::Label::new(&package.rustc).wrap());
    ui.label(format!(
        "{} file hashes match SHA256SUMS ({} bytes)",
        package.hashed_files, package.hashed_bytes
    ));
    ui.add(egui::Label::new(package.folder.display().to_string()).wrap());
    ui.label(
        "Local integrity check. Compare the original archive checksum with a source you trust.",
    );
    ui.label("Native protocol compatibility requires a separate check.");
}

#[cfg(test)]
mod tests;
