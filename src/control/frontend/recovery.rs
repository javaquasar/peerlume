//! One background recovery request at a time for the native frontends.
use super::Client;
use std::{
    io,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
};

enum Outcome {
    Attached(Client),
    Validation(Result<(), String>),
    AttachFailed(String),
}

pub struct Recovery {
    directory: PathBuf,
    message: String,
    startup_failure: Option<String>,
    pending: Option<Receiver<Outcome>>,
}

impl Recovery {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            message: String::new(),
            startup_failure: None,
            pending: None,
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub fn retry(&mut self) -> io::Result<()> {
        self.start("Connecting to the background client…", |directory| {
            match Client::connect_or_launch(&directory) {
                Ok(client) => Outcome::Attached(client),
                Err(error) => Outcome::AttachFailed(error),
            }
        })
    }

    pub fn validate(&mut self) -> io::Result<()> {
        self.start("Validating the profile configuration…", |directory| {
            Outcome::Validation(Client::validate_configuration(&directory))
        })
    }

    fn start(
        &mut self,
        message: &str,
        job: impl FnOnce(PathBuf) -> Outcome + Send + 'static,
    ) -> io::Result<()> {
        if self.busy() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Recovery is already in progress",
            ));
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let directory = self.directory.clone();
        std::thread::Builder::new()
            .name("peerlume-startup-recovery".into())
            .spawn(move || {
                let _ = sender.send(job(directory));
            })?;
        self.pending = Some(receiver);
        self.message = message.to_owned();
        Ok(())
    }

    pub fn poll(&mut self) -> Option<Client> {
        let result = self.pending.as_ref()?.try_recv();
        if matches!(result, Err(TryRecvError::Empty)) {
            return None;
        }
        self.pending = None;
        match result {
            Ok(Outcome::Attached(client)) => {
                self.message.clear();
                self.startup_failure = None;
                Some(client)
            }
            Ok(Outcome::Validation(result)) => {
                self.message = result.err().unwrap_or_else(|| {
                    "Configuration is valid. Retry to start or attach to the background client."
                        .into()
                });
                if let Some(previous) = &self.startup_failure {
                    self.message.push('\n');
                    self.message.push_str(previous);
                }
                None
            }
            Ok(Outcome::AttachFailed(error)) => {
                self.startup_failure = Some(error.clone());
                self.message = error;
                None
            }
            Err(_) => {
                self.message = "The recovery worker stopped before reporting a result.".into();
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_prevents_overlapping_requests_and_reports_the_worker_outcome() {
        let profile = tempfile::tempdir().unwrap();
        let mut recovery = Recovery::new(profile.path().to_owned());
        let (release, gate) = mpsc::channel();
        recovery
            .start("Checking", move |_| {
                gate.recv().unwrap();
                Outcome::Validation(Ok(()))
            })
            .unwrap();
        assert_eq!(
            recovery.validate().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(recovery.poll().is_none());
        assert_eq!(recovery.message(), "Checking");
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while recovery.busy() {
            assert!(std::time::Instant::now() < deadline);
            assert!(recovery.poll().is_none());
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(recovery.message().starts_with("Configuration is valid."));
        assert_eq!(profile.path().read_dir().unwrap().count(), 0);
    }
}
