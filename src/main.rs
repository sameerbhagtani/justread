#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // no console window on Windows

mod annot;
mod app;
mod commands;
mod dialogs;
mod find;
mod render;

use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;

fn main() -> eframe::Result {
    let start = Instant::now();
    let mut path = None;
    let mut bench = false;
    for arg in std::env::args_os().skip(1) {
        if arg == "--bench-startup" {
            bench = true;
        } else {
            path = Some(PathBuf::from(arg));
        }
    }

    let bench = bench.then_some(start);

    // Opening the PDF overlaps with window creation, which is most of our startup time.
    let renderer = render::Renderer::spawn();
    if let Some(path) = path {
        renderer.open(path);
    }

    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-128.png"))
        .expect("bundled icon is a valid PNG");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("JustRead")
            // Must match justread.desktop, or Wayland shows no icon.
            .with_app_id("justread")
            .with_icon(icon)
            .with_inner_size([900.0, 1000.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        // Names the settings folder; renaming it forgets saved window sizes.
        "justread",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, renderer, bench)))),
    )
}
