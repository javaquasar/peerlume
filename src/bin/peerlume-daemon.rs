//! Foreground session owner; service managers may own daemonization.
use peerlume::{
    client::{ClientBackend, SessionNotice, torrent_from_deferred},
    control::{
        Controller,
        frontend::{Client, Server},
        host::{Host, InstanceLock},
    },
    settings::{ClientSettings, data_directory},
    state::PersistenceIntent,
    torrent::EffectiveTorrentSettings,
};
use std::{
    collections::BTreeSet,
    io,
    path::Path,
    sync::{Arc, Mutex, TryLockError},
    time::{Duration, Instant},
};

#[derive(Default)]
struct Options {
    check_config: bool,
    enable_control: bool,
    stop: bool,
}

fn options(args: &[String]) -> io::Result<Options> {
    let mut options = Options::default();
    let mut profile = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--profile"
                if !profile
                    && args
                        .get(index + 1)
                        .is_some_and(|path| Path::new(path).is_absolute()) =>
            {
                profile = true;
                index += 2;
            }
            "--check-config" if !options.check_config => {
                options.check_config = true;
                index += 1;
            }
            "--enable-control" if !options.enable_control => {
                options.enable_control = true;
                index += 1;
            }
            "--stop" if !options.stop => {
                options.stop = true;
                index += 1;
            }
            _ => return Err(invalid_arguments()),
        }
    }
    if options.stop && (options.check_config || options.enable_control) {
        return Err(invalid_arguments());
    }
    Ok(options)
}

fn invalid_arguments() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "Invalid argument; use --help")
}

fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--version"] {
        println!("peerlume-daemon {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args == ["--help"] {
        println!(
            "PeerLume headless session owner\nUsage: peerlume-daemon [--profile ABSOLUTE_DIRECTORY] [--check-config] [--enable-control]\n       peerlume-daemon [--profile ABSOLUTE_DIRECTORY] --stop\nOne engine per profile. Ctrl+C / SIGTERM or --stop shuts down cleanly.\n--stop connects ONLY to an existing compatible owner and waits up to 95 seconds\nfor its profile lock to be released; it never starts or kills an engine.\nPublic agent control is disabled by default. --enable-control accepts ONLY\nexisting tokens granted through the trusted GUI/TUI; no permissions are granted here.\nLinux private tracker credentials require an unlocked Secret Service."
        );
        return Ok(());
    }
    let options = options(&args)?;
    peerlume::settings::configure_profile_from_args()?;
    let directory = std::path::absolute(data_directory())?;
    // Stop still works if saved configuration has become invalid. This branch
    // never loads settings or launches a replacement owner.
    if options.stop {
        return stop_existing(&directory);
    }
    let (settings, new_settings) = load_settings(&directory)?;
    validate_settings(&settings)?;
    if options.check_config {
        println!(
            "Configuration valid; no engine, profile lock, configuration write or network started"
        );
        return Ok(());
    }
    let _instance = InstanceLock::acquire(&directory)?;
    if new_settings {
        settings.save_to(&directory.join("settings.toml"))?;
    }
    let mut backend = ClientBackend::try_start(&settings).map_err(io::Error::other)?;
    let mut torrents: Vec<_> = backend
        .load_deferred_torrents()
        .map_err(io::Error::other)?
        .into_iter()
        .map(|record| torrent_from_deferred(record, &settings))
        .collect();
    for torrent in &mut torrents {
        let resolved =
            EffectiveTorrentSettings::resolve(&settings, &torrent.overrides, torrent.is_private);
        if !Path::new(&resolved.download_directory).is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "A restored torrent has a relative data directory; make it absolute before running as a service",
            ));
        }
        // Pin inherited storage before transfer startup; later global settings
        // edits must not implicitly move a cold restored torrent.
        if torrent.overrides.download_directory.is_none() {
            torrent.overrides.download_directory = Some(resolved.download_directory);
            backend
                .persist_torrent_update(torrent, PersistenceIntent::STORAGE, None)
                .map_err(io::Error::other)?;
        }
    }
    let mut control = Controller::new(backend, torrents);
    // Trusted native Access needs the service store even while the separate
    // public agent endpoint is disabled.
    control
        .configure_agents(
            &directory,
            settings.download.download_directory.clone().into(),
        )
        .map_err(|error| io::Error::other(format!("Control initialization failed: {error:?}")))?;
    control
        .set_agent_settings(&settings)
        .map_err(|error| io::Error::other(format!("Control settings failed: {error:?}")))?;
    if options.enable_control {
        let agent = control
            .agent_access()
            .ok_or_else(|| io::Error::other("Control unavailable"))?;
        for root in [&agent.import_root, &agent.export_root] {
            std::fs::create_dir_all(root)?;
        }
        agent.enabled = true;
    }
    let mut restore_queue = control.begin_restores();
    let diagnostic_wake = control.owner_diagnostic_wake();
    let owner = Arc::new(Mutex::new(control));
    let host = Host::start(owner.clone(), |control| control, directory.clone())?;
    let native = Server::start(owner.clone(), directory, settings.clone())?;
    println!("PeerLume daemon ready");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let outcome = runtime.block_on(async {
        let restore = async {
            while !restore_queue.is_empty() {
                let mut retry = false;
                {
                    match owner.try_lock() {
                        Ok(mut control) => {
                            for _ in 0..8 {
                                let Some(id) = restore_queue.front() else { break };
                                if control.admit_restored(id, &settings).map_err(io::Error::other)? {
                                    restore_queue.pop_front();
                                } else { retry = true; break; }
                            }
                        }
                        Err(TryLockError::WouldBlock) => retry = true,
                        Err(TryLockError::Poisoned(_)) => return Err(io::Error::other("Controller owner failed during restore")),
                    }
                }
                if retry { tokio::time::sleep(Duration::from_millis(10)).await; }
                else { tokio::task::yield_now().await; }
            }
            // Finishing registration admission leaves the background owner alive.
            std::future::pending::<io::Result<()>>().await
        };
        let signal = async {
            #[cfg(unix)] {
                let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
                tokio::select! { result = tokio::signal::ctrl_c() => result?, _ = terminate.recv() => {} }
            }
            #[cfg(not(unix))] tokio::signal::ctrl_c().await?;
            Ok::<_, io::Error>(())
        };
        let monitor = async {
            let mut reported = BTreeSet::new();
            loop {
                // Register before draining; boundary notices leave a permit.
                // This is independent of the transfer owner's wake token.
                let notified = diagnostic_wake.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if let Some(error) = *host.error.lock().unwrap_or_else(|error| error.into_inner()) {
                    return Err(io::Error::other(format!("Local control endpoint failed: {error:?}")));
                }
                let mut count = 0;
                let mut retry = false;
                match owner.try_lock() {
                    Ok(mut control) => {
                        for _ in 0..256 {
                            let Some(notice) = control.poll_next() else { break };
                            count += 1;
                            report_notice(notice, &mut reported)?;
                        }
                    }
                    Err(TryLockError::WouldBlock) => retry = true,
                    Err(TryLockError::Poisoned(_)) => return Err(io::Error::other("Controller owner failed")),
                }
                if count == 256 { tokio::task::yield_now().await; continue; }
                // Host endpoint errors have no notification yet: retain a slow
                // diagnostic deadline, independent of transfer event pumping.
                let delay = if retry { Duration::from_millis(10) } else { Duration::from_secs(5) };
                tokio::select! { _ = &mut notified => {}, _ = tokio::time::sleep(delay) => {} }
            }
        };
        tokio::select! { result = signal => result, result = monitor => result, result = restore => result, _ = native.stopped() => Ok(()) }
    });
    drop(native);
    drop(host);
    let stopped = owner
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .shutdown()
        .map_err(io::Error::other);
    // InstanceLock stays held through final counters, checkpoint and journal
    // barriers. The existing session is joined; no replacement is created.
    drop(owner);
    outcome.and(stopped)
}

fn load_settings(directory: &Path) -> io::Result<(ClientSettings, bool)> {
    match ClientSettings::load_from(&directory.join("settings.toml")) {
        Ok(settings) => Ok((settings, false)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if directory.join("state.sqlite3").exists() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "Existing profile has no settings.toml; restore its configuration before starting the owner",
                ));
            }
            let mut settings = ClientSettings::default();
            settings.download.download_directory =
                directory.join("downloads").to_string_lossy().into_owned();
            settings.download.incomplete_directory = directory
                .join("downloads/incomplete")
                .to_string_lossy()
                .into_owned();
            Ok((settings, true))
        }
        Err(error) => Err(error),
    }
}

fn validate_settings(settings: &ClientSettings) -> io::Result<()> {
    let issues = settings.validate();
    if !issues.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            issues
                .iter()
                .map(|issue| format!("{}: {}", issue.field, issue.message))
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    if !Path::new(&settings.download.download_directory).is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Daemon requires an absolute download.download_directory in settings.toml; relative paths depend on the service working directory",
        ));
    }
    // Capability checks construct options only; --check-config must reject a
    // known unsupported binding/proxy before claiming validity or starting I/O.
    peerlume::network_settings::options(settings).map_err(io::Error::other)?;
    Ok(())
}

fn stop_existing(directory: &Path) -> io::Result<()> {
    let client = Client::connect(directory).map_err(|error| {
        io::Error::other(format!(
            "Cannot stop an existing compatible owner: {error}. No owner was started or killed"
        ))
    })?;
    client.stop_owner().map_err(|error| {
        io::Error::other(format!("Existing owner rejected the stop request: {error}"))
    })?;
    drop(client);
    let deadline = Instant::now() + Duration::from_secs(95);
    loop {
        match InstanceLock::acquire(directory) {
            Ok(lock) => {
                drop(lock);
                println!("PeerLume owner stopped; profile lock released");
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Stop was requested but the owner still holds the profile lock after 95 seconds; no process was killed",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn report_notice(notice: SessionNotice, reported: &mut BTreeSet<String>) -> io::Result<()> {
    match notice {
        SessionNotice::Stopped => {
            return Err(io::Error::other("Transfer engine stopped unexpectedly"));
        }
        SessionNotice::Error { id, .. } => {
            if id.is_empty() {
                return Err(io::Error::other(
                    "Transfer engine failed; raw diagnostics withheld from service logs",
                ));
            }
            if reported.len() < 1024 && reported.insert(id.clone()) {
                eprintln!(
                    "Transfer {id}: engine reported an error; raw details withheld from service logs"
                );
                if reported.len() == 1024 {
                    eprintln!(
                        "Further unique transfer errors suppressed; inspect authorized status observations"
                    );
                }
            }
        }
        SessionNotice::StateApplied { id }
        | SessionNotice::Added { id }
        | SessionNotice::Removed { id } => {
            reported.remove(&id);
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stop_is_exclusive_and_never_implies_startup_options() {
        assert!(options(&["--stop".into()]).unwrap().stop);
        for extra in ["--enable-control", "--check-config", "--stop"] {
            assert!(options(&["--stop".into(), extra.into()]).is_err());
        }
    }
    #[test]
    fn new_profile_defaults_are_absolute_and_loading_does_not_write() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("missing-profile");
        let (settings, missing) = load_settings(&directory).unwrap();
        assert!(missing);
        assert!(Path::new(&settings.download.download_directory).is_absolute());
        validate_settings(&settings).unwrap();
        assert!(!directory.exists());
    }
    #[test]
    fn existing_relative_configuration_is_rejected_without_rewriting() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("settings.toml");
        let settings = ClientSettings::default();
        settings.save_to(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        let (loaded, missing) = load_settings(root.path()).unwrap();
        assert!(!missing);
        assert!(validate_settings(&loaded).is_err());
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
    #[test]
    fn an_existing_profile_without_configuration_is_not_given_new_roots() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("state.sqlite3"), b"synthetic marker").unwrap();
        assert_eq!(
            load_settings(root.path()).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(!root.path().join("settings.toml").exists());
    }

    #[test]
    fn unsupported_ip_binding_is_rejected_by_pure_configuration_check() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("missing-profile");
        let (mut settings, _) = load_settings(&directory).unwrap();
        settings.connection.bind_address = "127.0.0.1".into();
        let error = validate_settings(&settings).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("IP-address binding is not supported")
        );
        assert!(!directory.exists());
    }
}
