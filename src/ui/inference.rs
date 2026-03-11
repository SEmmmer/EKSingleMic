use eframe::egui;

use crate::{
    app::state::AppState,
    config::settings::InferenceMode,
};

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    ui.heading("推理页");
    ui.label("M4 前先保留模式切换与状态展示骨架。");
    ui.separator();

    ui.label("模式选择");
    ui.radio_value(
        &mut state.settings.inference_mode,
        InferenceMode::Passthrough,
        InferenceMode::Passthrough.label(),
    );
    ui.radio_value(
        &mut state.settings.inference_mode,
        InferenceMode::BasicFilter,
        InferenceMode::BasicFilter.label(),
    );
    ui.radio_value(
        &mut state.settings.inference_mode,
        InferenceMode::StrongIsolation,
        InferenceMode::StrongIsolation.label(),
    );

    ui.separator();
    ui.label("实时状态: 尚未启动音频链路");
    ui.label("目标: 先完成 M0 GUI 壳子，再进入 M1 音频设备与直通。");
}
