fn main() -> std::io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.as_slice() == ["--version"] {
        println!("peerlume-tui {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.as_slice() == ["--help"] {
        println!(
            "PeerLume terminal client\nUsage: peerlume-tui [--profile ABSOLUTE_DIRECTORY]\nF1: help; F8: opt-in agent access. One owner per profile.\nLinux private tracker credentials require an unlocked Secret Service keyring."
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
    peerlume::tui::run()
}
