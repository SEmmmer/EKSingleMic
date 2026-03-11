mod app;
mod audio;
mod config;
mod ml;
mod pipeline;
mod profile;
mod ui;
mod util;

use anyhow::{Context, anyhow};
use eframe::egui;

fn main() -> anyhow::Result<()> {
    app::init_logging().context("failed to initialize tracing")?;
    tracing::info!("starting EKSingleMic");

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 780.0])
            .with_min_inner_size([960.0, 640.0])
            .with_title("EKSingleMic"),
        ..Default::default()
    };

    eframe::run_native(
        "EKSingleMic",
        native_options,
        Box::new(|creation_context| {
            let app = app::SingleMicApp::bootstrap(creation_context)
                .context("failed to bootstrap application state")?;
            Ok(Box::new(app))
        }),
    )
    .map_err(|error| anyhow!("failed to run native eframe application: {error}"))?;

    Ok(())
}
