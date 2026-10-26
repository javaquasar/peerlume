use peerlume::app::HostedGui;

fn main() -> eframe::Result<()> {
    peerlume::settings::configure_profile_from_args()
        .map_err(|error| eframe::Error::AppCreation(Box::new(error)))?;
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

    let viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([1_080.0, 720.0])
        .with_min_inner_size([820.0, 560.0])
        .with_icon(peerlume::app::application_icon());
    #[cfg(target_os = "linux")]
    let viewport = viewport.with_app_id("peerlume");
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "PeerLume",
        options,
        Box::new(|_creation_context| Ok(Box::new(HostedGui::start()?))),
    )
}
