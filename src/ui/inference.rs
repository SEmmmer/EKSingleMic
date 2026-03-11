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
    ui.label("默认 profile 状态");
    if let Some(summary) = &state.default_profile_summary {
        ui.label(summary.label());
        if summary.is_metadata_only() {
            ui.colored_label(
                egui::Color32::from_rgb(220, 170, 90),
                "当前默认 profile 只有训练元数据，speaker embedding 还未就绪。",
            );
        } else {
            ui.colored_label(
                egui::Color32::from_rgb(70, 170, 90),
                format!(
                    "当前默认 profile 已就绪：{} 个 embedding | 阈值建议 {:.3}",
                    summary.embedding_count, summary.suggested_threshold
                ),
            );
        }
        ui.label(format!(
            "模型版本: {} | 质量: {} | 创建时间: {}",
            summary.model_version, summary.quality_severity, summary.created_at_utc
        ));
    } else {
        ui.colored_label(
            egui::Color32::from_rgb(220, 90, 90),
            "当前还没有可用的默认 profile，请先完成训练。",
        );
    }

    ui.separator();
    ui.label("实时状态: 尚未启动音频链路");
    ui.label("当前阶段重点：先打通默认 profile 的 embedding 注册，再继续进入实时 Basic Filter。");
}
