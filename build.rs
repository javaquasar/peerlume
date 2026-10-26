fn main() {
    println!("cargo:rerun-if-changed=assets/icons/peerlume.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var_os("CARGO_FEATURE_GUI").is_some()
    {
        winresource::WindowsResource::new()
            .set_icon("assets/icons/peerlume.ico")
            .set("ProductName", "PeerLume")
            .set("FileDescription", "PeerLume BitTorrent client")
            .compile()
            .expect("Could not embed the PeerLume Windows application icon");
    }
}
