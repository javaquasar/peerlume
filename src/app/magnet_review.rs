//! GUI magnet review uses the background owner's cancellable metadata lookup.
use super::*;
use crate::control::frontend::{
    ConnectionStatus, ConnectionView, MagnetReviewTask, MagnetReviewToken, MagnetSelection, Receipt,
};

pub(super) struct Pending {
    task: MagnetReviewTask,
    owner: String,
}

pub(super) struct ReviewedSource {
    token: MagnetReviewToken,
    owner: String,
    pub has_trackers: bool,
    pub commit_job: Option<u64>,
    pub needs_review: bool,
}

impl ReviewedSource {
    pub fn can_confirm(&self, connection: &ConnectionView) -> bool {
        connection.status == ConnectionStatus::Connected
            && connection.owner_instance.as_deref() == Some(self.owner.as_str())
            && !self.needs_review
            && self.commit_job.is_none()
    }
}

impl PeerLumeApp {
    pub(super) fn add_magnet(&mut self) {
        if self.pending_magnet.is_some()
            || self.metainfo_loader.is_loading()
            || self.add_dialog.is_some()
        {
            self.add_status = "Finish or cancel the current torrent review first".into();
            return;
        }
        let uri = self.magnet_input.trim();
        let Some(client) = self.control.backend.attached_client() else {
            self.add_status =
                "Connect to the background client before requesting magnet metadata".into();
            return;
        };
        let connection = client.connection();
        let Some(owner) = connection
            .owner_instance
            .filter(|_| connection.status == ConnectionStatus::Connected)
        else {
            self.add_status =
                "Connect to the background client before requesting magnet metadata".into();
            return;
        };
        match MagnetReviewTask::start(client, uri.to_owned()) {
            Ok(task) => {
                self.pending_magnet = Some(Pending { task, owner });
                self.magnet_input.clear();
                self.add_status = "Requesting magnet metadata only".into();
            }
            Err(error) => self.desktop_error("Read magnet metadata", None, error),
        }
    }

    pub(super) fn poll_magnet_review(&mut self) {
        if self.pending_magnet.is_none()
            && !self.add_dialog.as_ref().is_some_and(|dialog| matches!(&dialog.source, AddSource::Magnet(source) if source.commit_job.is_some()))
        {
            return;
        }
        let connection = self.desktop_connection();
        if self.pending_magnet.as_ref().is_some_and(|pending| {
            connection
                .owner_instance
                .as_deref()
                .is_some_and(|owner| owner != pending.owner)
        }) {
            self.pending_magnet = None;
            self.desktop_error(
                "Read magnet metadata",
                None,
                "The background owner changed. Request metadata again.".into(),
            );
        }
        if let Some(dialog) = &mut self.add_dialog
            && let AddSource::Magnet(source) = &mut dialog.source
            && source.commit_job.is_some()
            && connection
                .owner_instance
                .as_deref()
                .is_some_and(|owner| owner != source.owner)
        {
            source.commit_job = None;
            source.needs_review = true;
            dialog.error = "The background owner changed while confirming. Inspect the torrent list before retrying.".into();
        }
        let Some(result) = self
            .pending_magnet
            .as_ref()
            .and_then(|pending| pending.task.poll())
        else {
            return;
        };
        let pending = self
            .pending_magnet
            .take()
            .expect("result for the current lookup");
        match result {
            Ok(review) => {
                let download_directory = self
                    .last_download_directory
                    .clone()
                    .unwrap_or_else(|| self.settings.download.download_directory.clone());
                let source = ReviewedSource {
                    token: review.token,
                    owner: pending.owner,
                    has_trackers: !review.metadata.trackers.is_empty(),
                    commit_job: None,
                    needs_review: false,
                };
                self.add_dialog = Some(AddTorrentDialog::from_source(
                    AddSource::Magnet(source),
                    review.metadata,
                    &self.settings,
                    download_directory,
                ));
                self.add_status = "Magnet metadata ready for review".into();
            }
            Err(error) => self.desktop_error("Read magnet metadata", None, error),
        }
    }

    pub(super) fn show_magnet_lookup(&mut self, ctx: &egui::Context) {
        if self.pending_magnet.is_none() {
            return;
        }
        if lookup_modal(
            ctx,
            self.desktop_connection().status == ConnectionStatus::Connected,
        ) {
            self.pending_magnet = None;
            self.add_status = "Stopped waiting for magnet metadata".into();
            self.open_next_queued_torrent();
        } else {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    pub(super) fn confirm_magnet_review(&mut self) {
        let connection = self.desktop_connection();
        let Some(dialog) = self.add_dialog.as_mut() else {
            return;
        };
        let overrides = dialog.transfer_overrides();
        let AddSource::Magnet(source) = &mut dialog.source else {
            return;
        };
        if !source.can_confirm(&connection) {
            return;
        }
        match source.token.confirm(MagnetSelection {
            files: &dialog.files,
            peer_id: dialog.peer_id.clone(),
            overrides,
            start: dialog.start_immediately,
            audit: None,
        }) {
            Ok(job_id) => {
                source.commit_job = Some(job_id);
                dialog.error.clear();
                self.add_status = "Awaiting background owner registration".into();
            }
            Err(error) => dialog.error = crate::runtime_status::display_error(&error),
        }
    }

    pub(super) fn apply_magnet_receipt(&mut self, receipt: &Receipt) {
        if receipt.queued || receipt.label != "Magnet registration" {
            return;
        }
        let Some(dialog) = self.add_dialog.as_mut() else {
            return;
        };
        let AddSource::Magnet(source) = &mut dialog.source else {
            return;
        };
        if source.commit_job != Some(receipt.job_id) || source.owner != receipt.owner_instance {
            return;
        }
        source.commit_job = None;
        if receipt.ambiguous_error {
            source.needs_review = true;
            dialog.error =
                "The registration outcome is unknown. Inspect the torrent list before retrying."
                    .into();
        } else if let Some(error) = &receipt.error {
            dialog.error = crate::runtime_status::display_error(error);
        } else {
            source.token.confirmed();
            self.last_download_directory = Some(dialog.download_directory.clone());
            self.add_dialog = None;
        }
    }

    pub(super) fn mark_magnet_receipt_loss(&mut self) {
        if let Some(dialog) = &mut self.add_dialog
            && let AddSource::Magnet(source) = &mut dialog.source
            && source.commit_job.take().is_some()
        {
            source.needs_review = true;
            dialog.error =
                "The registration reply was lost. Inspect the torrent list before retrying.".into();
        }
    }
}

fn lookup_modal(ctx: &egui::Context, connected: bool) -> bool {
    let mut cancelled = false;
    let modal = egui::Modal::new(egui::Id::new("magnet_metadata_lookup")).show(ctx, |ui| {
        ui.heading("Retrieving magnet metadata");
        ui.spinner();
        ui.label("PeerLume is retrieving metadata. File downloads wait for your confirmation.");
        if !connected {
            ui.label("Background client disconnected. Waiting for the same owner to reconnect.");
        }
        cancelled = ui.button("Cancel lookup").clicked();
    });
    cancelled || modal.should_close()
}
