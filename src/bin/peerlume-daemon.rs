//! Foreground service owner; systemd/Windows service managers own daemonization.
use peerlume::{
    client::{ClientBackend, add_torrent_command, torrent_from_record},
    control::{
        Controller,
        host::{Host, InstanceLock},
    },
    settings::{ClientSettings, data_directory},
};
use std::{
    io,
    path::Path,
    sync::{Arc, Mutex},
};

fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--version"] {
        println!("peerlume-daemon {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args == ["--help"] {
        println!(
            "PeerLume headless session owner\nUsage: peerlume-daemon [--profile ABSOLUTE_DIRECTORY] [--check-config] [--enable-control]\nOne engine per profile. Ctrl+C / SIGTERM shuts down cleanly.\nControl is disabled by default. --enable-control accepts ONLY existing tokens\ngranted through the trusted GUI/TUI; no new permissions are granted here.\nLinux private tracker credentials require an unlocked Secret Service."
        );
        return Ok(());
    }
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--profile"
                if args
                    .get(index + 1)
                    .is_some_and(|path| Path::new(path).is_absolute()) =>
            {
                index += 2
            }
            "--check-config" | "--enable-control" => index += 1,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Invalid argument; use --help",
                ));
            }
        }
    }
    peerlume::settings::configure_profile_from_args()?;
    let directory = data_directory();
    let settings = match ClientSettings::load_from(&directory.join("settings.toml")) {
        Ok(settings) => settings,
        Err(error) if error.kind() == io::ErrorKind::NotFound => ClientSettings::default(),
        Err(error) => return Err(error),
    };
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
    if args.iter().any(|arg| arg == "--check-config") {
        println!("Configuration valid; no engine, profile lock or network started");
        return Ok(());
    }
    let _instance = InstanceLock::acquire(&directory)?;
    let backend = ClientBackend::try_start(&settings).map_err(io::Error::other)?;
    let torrents: Vec<_> = backend
        .load_torrents()
        .map_err(io::Error::other)?
        .into_iter()
        .map(|record| torrent_from_record(record, &settings))
        .collect();
    if torrents.iter().any(|torrent| {
        !Path::new(
            &peerlume::torrent::EffectiveTorrentSettings::resolve(
                &settings,
                &torrent.overrides,
                torrent.is_private,
            )
            .download_directory,
        )
        .is_absolute()
    }) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "A restored torrent has a relative data directory; make it absolute before running as a service",
        ));
    }
    for torrent in &torrents {
        backend
            .send(add_torrent_command(torrent, &settings))
            .map_err(io::Error::other)?;
    }
    let mut control = Controller::new(backend, torrents);
    if args.iter().any(|arg| arg == "--enable-control") {
        control
            .configure_agents(
                &directory,
                settings.download.download_directory.clone().into(),
            )
            .map_err(|e| io::Error::other(format!("Control initialization failed: {e:?}")))?;
        control
            .set_agent_settings(&settings)
            .map_err(|e| io::Error::other(format!("Control settings failed: {e:?}")))?;
        let agent = control
            .agent_access()
            .ok_or_else(|| io::Error::other("Control unavailable"))?;
        for root in [&agent.import_root, &agent.export_root] {
            std::fs::create_dir_all(root)?;
        }
        agent.enabled = true;
    }
    let owner = Arc::new(Mutex::new(control));
    let host = Host::start(owner.clone(), |control| control, directory)?;
    // Signals and bounded diagnostics only; the shared Host pumps the one owner.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let outcome=runtime.block_on(async {
        let signal=async {
        #[cfg(unix)]
        {
            let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! { result = tokio::signal::ctrl_c() => result?, _ = terminate.recv() => {} }
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await?;
        Ok::<_, io::Error>(())
        };
        let monitor=async {
            let mut ticker=tokio::time::interval(std::time::Duration::from_millis(500));
            let mut reported=std::collections::BTreeSet::new();
            loop {
                ticker.tick().await;
                if let Some(error)=*host.error.lock().unwrap_or_else(|error|error.into_inner()){return Err(io::Error::other(format!("Local control endpoint failed: {error:?}")));}
                let mut control=match owner.try_lock(){Ok(control)=>control,Err(std::sync::TryLockError::WouldBlock)=>continue,Err(_)=>return Err(io::Error::other("Controller owner failed"))};
                for _ in 0..256 {
                    let Some(notice)=control.poll_next() else{break;};
                    match notice {
                        peerlume::client::SessionNotice::Stopped=>return Err(io::Error::other("Transfer engine stopped unexpectedly")),
                        peerlume::client::SessionNotice::Error{id,..}=>{
                            if id.is_empty(){return Err(io::Error::other("Transfer engine failed; raw diagnostics withheld from service logs"));}
                            if reported.len()<1024 && reported.insert(id.clone()){
                                eprintln!("Transfer {id}: engine reported an error; raw details withheld from service logs");
                                if reported.len()==1024{eprintln!("Further unique transfer errors suppressed; inspect authorized status observations");}
                            }
                        }
                        peerlume::client::SessionNotice::StateApplied{id}|peerlume::client::SessionNotice::Added{id}|peerlume::client::SessionNotice::Removed{id}=>{reported.remove(&id);}
                        _=>{}
                    }
                }
            }
        };
        tokio::select!{result=signal=>result,result=monitor=>result}
    });
    drop(host);
    let stopped = owner
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .shutdown()
        .map_err(io::Error::other);
    // Dropping the sole controller joins the shared session's clean shutdown;
    // it never starts a replacement engine or deletes payload data.
    drop(owner);
    outcome.and(stopped)
}
