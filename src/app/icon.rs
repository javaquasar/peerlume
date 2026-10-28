//! One embedded icon for the native window on Windows and Linux.

pub fn application_icon() -> eframe::egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../../assets/icons/peerlume-256.png"))
        .expect("The embedded PeerLume icon must be a valid PNG")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_icon_has_valid_rgba_dimensions_transparency_and_visible_content() {
        let icon = application_icon();
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
        for (x, y) in [(0, 0), (255, 0), (0, 255), (255, 255)] {
            assert_eq!(icon.rgba[(y * 256 + x) * 4 + 3], 0);
        }
        let (pixels, remainder) = icon.rgba.as_chunks::<4>();
        assert!(remainder.is_empty());
        let opaque = pixels.iter().filter(|pixel| pixel[3] > 240).count();
        assert!(opaque > 256 * 256 / 4, "The icon must not be empty");
        assert!(
            opaque < 256 * 256 * 3 / 4,
            "Keep transparent negative space"
        );
        assert!(
            pixels
                .iter()
                .any(|pixel| pixel[3] > 240 && pixel[2] > 200 && pixel[0] < 80)
        );
    }
}
