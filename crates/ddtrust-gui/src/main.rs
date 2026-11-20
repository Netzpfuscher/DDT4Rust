#![cfg_attr(windows, windows_subsystem = "windows")]

mod agent;
mod app;
mod assets;
mod screen_view;
mod worker;

use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .without_time()
        .init();

    let mut data: Option<PathBuf> = None;
    let mut simulate = false;
    let mut open_ecu: Option<String> = None;
    let mut open_screen: Option<String> = None;
    let mut open_tab: Option<String> = None;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if a == "--simulate" {
            simulate = true;
        } else if a == "--ecu" && i + 1 < args.len() {
            open_ecu = Some(args[i + 1].clone());
            i += 1;
        } else if a == "--screen" && i + 1 < args.len() {
            open_screen = Some(args[i + 1].clone());
            i += 1;
        } else if a == "--tab" && i + 1 < args.len() {
            open_tab = Some(args[i + 1].clone());
            i += 1;
        } else if !a.starts_with('-') {
            data = Some(PathBuf::from(a));
        }
        i += 1;
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 920.0])
            .with_min_inner_size([980.0, 640.0])
            .with_title("DDTRust — Renault / Dacia diagnostics"),
        ..Default::default()
    };
    eframe::run_native(
        "DDTRust",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::App::new(
                cc,
                data,
                simulate,
                open_ecu.clone(),
                open_screen.clone(),
                open_tab.clone(),
            )))
        }),
    )
}
