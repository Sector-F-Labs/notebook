//! Notebook: WYSIWYG markdown notes for a folder of `.md` files, built on
//! egui and Patina.

mod app;
mod config;
mod doc;
mod editor;
mod embeds;
mod fonts;
mod markdown;
mod notes;
mod screenshot;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let open = std::env::args_os().nth(1).map(PathBuf::from).map(|p| p.canonicalize().unwrap_or(p));
    let config = config::Config::load();
    let options = eframe::NativeOptions {
        viewport: patina::frame::viewport(
            egui::ViewportBuilder::default()
                .with_title("Notebook")
                .with_app_id("com.sectorflabs.Notebook")
                .with_inner_size([1100.0, 740.0])
                .with_min_inner_size([560.0, 360.0]),
            !config.system_title_bar,
        ),
        ..Default::default()
    };
    eframe::run_native("notebook", options, Box::new(|cc| Ok(Box::new(app::App::new(cc, open)))))
}
