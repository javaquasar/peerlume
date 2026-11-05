fn main() -> std::io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.iter().any(|argument| argument == "--attach-check") {
        let filtered: Vec<_> = args
            .iter()
            .filter(|argument| argument.as_str() != "--attach-check")
            .collect();
        if !(filtered.is_empty()
            || filtered.len() == 2
                && filtered[0] == "--profile"
                && std::path::Path::new(filtered[1]).is_absolute())
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Use --attach-check [--profile ABSOLUTE_DIRECTORY]",
            ));
        }
        peerlume::settings::configure_profile_from_args()?;
        return peerlume::control::frontend::attach_check(&peerlume::settings::data_directory());
    }
    if args.as_slice() == ["--version"] {
        println!("peerlume-tui {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.as_slice() == ["--help"] {
        println!(
            "PeerLume terminal client\nUsage: peerlume-tui [--profile ABSOLUTE_DIRECTORY] [--attach-check]\nF1: help; F8: opt-in agent access. Closing the frontend keeps the background owner running.\nLinux private tracker credentials require an unlocked Secret Service keyring."
        );
        return Ok(());
    }
    if !(args.is_empty()
        || args.len() == 2
            && args[0] == "--profile"
            && std::path::Path::new(&args[1]).is_absolute())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Use --help, --version, or --profile ABSOLUTE_DIRECTORY",
        ));
    }
    let outcome = peerlume::tui::run();
    let drained = peerlume::audit_workers::finish_after_close().map_err(std::io::Error::other);
    outcome.and(drained)
}
