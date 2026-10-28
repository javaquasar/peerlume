//! Construct the real engine configuration. Never silently bypass a requested proxy.
use crate::settings::{ClientSettings, TrackerReportingMode};
use librqbit::{
    ConnectionOptions, ListenerMode, ListenerOptions, PeerConnectionOptions, SessionOptions,
};
use std::{num::NonZeroU32, time::Duration};

pub fn options(settings: &ClientSettings) -> Result<SessionOptions, String> {
    let connection = &settings.connection;
    if !connection.bind_address.trim().is_empty() {
        return Err("IP-address binding is not supported; use a network interface instead".into());
    }
    let interface = connection.network_interface.trim();
    #[cfg(windows)]
    if !interface.is_empty() {
        return Err(
            "Interface binding for every transport is unsupported on Windows; engine not started"
                .into(),
        );
    }
    let proxy = connection.proxy_url.trim();
    if !proxy.is_empty() {
        let url = url::Url::parse(proxy).map_err(|_| "Invalid proxy URL")?;
        if url.scheme() != "socks5" || url.host_str().is_none() || url.port().is_none() {
            return Err("The engine requires socks5://HOST:PORT".into());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("Proxy credentials in settings are unsupported; use a local authenticated proxy bridge".into());
        }
        if !connection.proxy_peer_connections {
            return Err(
                "Enable proxy_peer_connections: the proxy must route both peers and HTTP trackers"
                    .into(),
            );
        }
        if !interface.is_empty() {
            return Err(
                "Combining proxy and interface binding is unsupported; engine not started".into(),
            );
        }
    }
    let limit = |kib: u64| -> Result<Option<NonZeroU32>, String> {
        let bytes = kib
            .checked_mul(1024)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or("Rate limit exceeds the engine range")?;
        Ok(NonZeroU32::new(bytes))
    };
    let mode = match (connection.tcp_enabled, connection.utp_enabled) {
        (true, true) => ListenerMode::TcpAndUtp,
        (true, false) => ListenerMode::TcpOnly,
        (false, true) => ListenerMode::UtpOnly,
        _ if !proxy.is_empty() => ListenerMode::TcpOnly,
        _ => return Err("At least one peer transport is required".into()),
    };
    // A proxy is not a VPN. Disable incoming and direct UDP discovery paths.
    Ok(SessionOptions {
        disable_trackers: settings.privacy.tracker_reporting == TrackerReportingMode::Disabled,
        bind_device_name: (!interface.is_empty()).then(|| interface.to_owned()),
        dht: (proxy.is_empty() && settings.bittorrent.dht).then(Default::default),
        disable_local_service_discovery: !proxy.is_empty() || !settings.bittorrent.lsd,
        listen: proxy.is_empty().then(|| ListenerOptions {
            mode,
            listen_addr: (
                "::".parse::<std::net::IpAddr>().unwrap(),
                if connection.random_port_on_start {
                    0
                } else {
                    connection.listen_port
                },
            )
                .into(),
            enable_upnp_port_forwarding: connection.port_forwarding,
            ..Default::default()
        }),
        connect: Some(ConnectionOptions {
            enable_tcp: connection.tcp_enabled,
            proxy_url: (!proxy.is_empty()).then(|| proxy.to_owned()),
            peer_opts: Some(PeerConnectionOptions {
                connect_timeout: Some(Duration::from_secs(connection.connection_timeout_seconds)),
                ..Default::default()
            }),
        }),
        ratelimits: librqbit::limits::LimitsConfig {
            download_bps: limit(settings.speed.download_limit_kib)?,
            upload_bps: limit(settings.speed.upload_limit_kib)?,
        },
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_engine_honors_aggregate_caps_and_proxy_peer_routes_without_udp_tracker_leaks() {
        use sha1::{Digest, Sha1};
        use std::{
            io::{Read, Write},
            sync::{
                Arc,
                atomic::{AtomicBool, AtomicUsize, Ordering},
            },
            time::Instant,
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let proxy = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let http = Arc::new(AtomicUsize::new(0));
        let peer = Arc::new(AtomicUsize::new(0));
        struct Recorder {
            stop: Arc<AtomicBool>,
            worker: Option<std::thread::JoinHandle<()>>,
        }
        impl Drop for Recorder {
            fn drop(&mut self) {
                self.stop.store(true, Ordering::Relaxed);
                if let Some(worker) = self.worker.take() {
                    worker.join().unwrap();
                }
            }
        }
        let (shutdown, requests, connections) = (stop.clone(), http.clone(), peer.clone());
        let worker = std::thread::spawn(move || {
            let mut workers = Vec::new();
            while !shutdown.load(Ordering::Relaxed) {
                if let Ok((mut socket, _)) = listener.accept() {
                    let (requests, connections) = (requests.clone(), connections.clone());
                    workers.push(std::thread::spawn(move||->std::io::Result<()>{
                        // Windows accepted sockets inherit the listener's nonblocking mode.
                        socket.set_nonblocking(false)?;
                        socket.set_read_timeout(Some(Duration::from_secs(1)))?;
                        socket.set_write_timeout(Some(Duration::from_secs(1)))?;
                        let mut greeting=[0u8;2];socket.read_exact(&mut greeting)?;
                        if greeting[0]!=5 {return Err(std::io::Error::other("Not SOCKS5"));}
                        let mut methods=vec![0;greeting[1] as usize];socket.read_exact(&mut methods)?;
                        socket.write_all(&[5,0])?;
                        let mut request=[0u8;4];socket.read_exact(&mut request)?;
                        let length=match request[3] {1=>4,4=>16,3=>{let mut length=[0];socket.read_exact(&mut length)?;length[0] as usize},_=>return Err(std::io::Error::other("Invalid SOCKS address"))};
                        let mut address=vec![0;length+2];socket.read_exact(&mut address)?;
                        let port=u16::from_be_bytes([address[length],address[length+1]]);
                        socket.write_all(&[5,0,0,1,127,0,0,1,0,1])?;
                        if port==1 {connections.fetch_add(1,Ordering::Relaxed);return Ok(());}
                        let mut header=Vec::new();
                        let mut byte=[0];
                        while header.len()<8192 && !header.ends_with(b"\r\n\r\n") {socket.read_exact(&mut byte)?;header.push(byte[0]);}
                        if !header.starts_with(b"GET ") {return Err(std::io::Error::other("Not HTTP GET"));}
                        requests.fetch_add(1,Ordering::Relaxed);
                        let mut body=b"d8:intervali3600e5:peers6:".to_vec();
                        body.extend([127,0,0,1,0,1]);body.push(b'e');
                        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes())?;
                        socket.write_all(&body)
                    }));
                } else {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            for worker in workers {
                if let Ok(Err(error)) = worker.join() {
                    eprintln!("Synthetic proxy stream failed: {error}");
                }
            }
        });
        let _recorder = Recorder {
            stop,
            worker: Some(worker),
        };
        let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        udp.set_read_timeout(Some(Duration::from_millis(250)))
            .unwrap();
        let mut settings = ClientSettings::default();
        settings.connection.proxy_url = format!("socks5://{proxy}");
        settings.connection.proxy_peer_connections = true;
        settings.speed.download_limit_kib = 123;
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::session::SessionRuntime::start_with_options(
            64,
            root.path().into(),
            options(&settings).unwrap(),
        )
        .unwrap();
        assert_eq!(
            runtime
                .test_engine()
                .unwrap()
                .ratelimits
                .get_download_bps()
                .unwrap()
                .get(),
            123 * 1024
        );
        let mut info = b"d6:lengthi1e4:name1:x12:piece lengthi16384e6:pieces20:".to_vec();
        info.extend_from_slice(&Sha1::digest(b"x"));
        info.push(b'e');
        let id = format!("v1: {:x}", Sha1::digest(&info));
        let mut bytes = b"d4:info".to_vec();
        bytes.extend(info);
        bytes.push(b'e');
        let command = crate::session::SessionCommand::AddTorrent {
            resume: Default::default(),
            transfer: Default::default(),
            repair_policy: Default::default(),
            verify_after_download: false,
            download_mode: crate::download_mode::DownloadMode::Normal,
            freeze_after_bytes: 0,
            upload_ratio: Default::default(),
            full_size: 1,
            id,
            source: "missing.torrent".into(),
            output_folder: root.path().display().to_string(),
            only_files: None,
            trackers: vec![
                "http://127.0.0.1:9/announce".into(),
                format!("udp://{}/announce", udp.local_addr().unwrap()),
            ],
            paused: false,
            peer_limit: Some(2),
        };
        runtime
            .try_send(crate::session::SessionCommand::PreparedAdd {
                command: Box::new(command),
                prepared: crate::session::PreparedTorrent {
                    bytes: bytes.into(),
                    directory: crate::storage::sandbox::Directory::open(root.path()).unwrap(),
                },
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last = None;
        while peer.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
            while let Some(event) = runtime.try_recv() {
                if let crate::session::SessionEvent::Error { message, .. } = &event {
                    panic!("Proxy fixture engine error: {message}");
                }
                last = Some(event);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            http.load(Ordering::Relaxed) > 0,
            "No HTTP proxy exchange; last engine event: {last:?}; journal: {:?}",
            runtime.journal.entries(None)
        );
        assert!(
            peer.load(Ordering::Relaxed) > 0,
            "Peer connection did not use SOCKS5"
        );
        assert!(
            udp.recv(&mut [0; 64]).is_err(),
            "UDP tracker traffic leaked outside SOCKS5"
        );
        runtime.shutdown().unwrap();
    }
    #[test]
    fn global_limits_and_transport_settings_reach_the_engine() {
        let mut settings = ClientSettings::default();
        settings.speed.download_limit_kib = 123;
        settings.connection.listen_port = 50123;
        settings.bittorrent.dht = false;
        let options = options(&settings).unwrap();
        assert_eq!(options.ratelimits.download_bps.unwrap().get(), 123 * 1024);
        assert_eq!(options.listen.unwrap().listen_addr.port(), 50123);
        assert!(options.dht.is_none());
    }
    #[test]
    fn proxy_configuration_is_fail_closed_and_has_no_direct_discovery() {
        let mut settings = ClientSettings::default();
        settings.connection.proxy_url = "socks5://127.0.0.1:1080".into();
        assert!(options(&settings).is_err());
        settings.connection.proxy_peer_connections = true;
        let configured = options(&settings).unwrap();
        assert!(configured.listen.is_none());
        assert!(configured.dht.is_none());
        assert!(configured.disable_local_service_discovery);
        settings.connection.proxy_url = "http://user:secret@localhost:8000".into();
        let error = options(&settings).err().unwrap();
        assert!(!error.contains("secret"));
        settings.connection.bind_address = "127.0.0.1".into();
        assert!(options(&settings).is_err());
    }
}
