use peerlume::app::PeerLumeApp;

fn main() -> eframe::Result<()> {
    if std::env::args().any(|argument| argument == "--update-blocklist") {
        let settings = peerlume::settings::ClientSettings::load();
        match peerlume::blocklist::download_and_cache(&settings.privacy.blocklist_url) {
            Ok(update) => println!(
                "Blocklist updated: {} rules, {} merged ranges, {} compressed bytes",
                update.blocklist.rule_count(),
                update.blocklist.range_count(),
                update.downloaded_bytes
            ),
            Err(error) => eprintln!("Blocklist update failed: {error}"),
        }
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1_080.0, 720.0])
            .with_min_inner_size([820.0, 560.0]),
        ..Default::default()
    };

    eframe::run_native(
        "PeerLume",
        options,
        Box::new(|_creation_context| Ok(Box::new(PeerLumeApp::default()))),
    )
}
