use eframe::egui;

use crate::{
    app::state::AppState, config::settings::InferenceMode, pipeline::realtime::RuntimeStage,
};

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    ui.heading("推理页");
    ui.label("M4 当前正在把实时 `Basic Filter` 接进实际音频链路。");
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
    ui.label(format!("实时状态: {}", state.runtime_stage.label()));
    if matches!(state.runtime_stage, RuntimeStage::RunningBasicFilter) {
        ui.label(format!(
            "当前 speaker score: {:.3} | 当前增益: {:.3}",
            state.runtime_metrics.current_similarity, state.runtime_metrics.current_frame_gain
        ));
        ui.label(format!(
            "最近 chunk 活动帧: {} / {}",
            state.runtime_metrics.last_chunk_active_frames,
            state.runtime_metrics.last_chunk_analyzed_frames
        ));
    } else if matches!(state.settings.inference_mode, InferenceMode::BasicFilter) {
        ui.label("当前已选中 Basic Filter；请到设备页启动实时链路。");
    } else {
        ui.label("当前可先用 Passthrough 校验设备链路，再切到 Basic Filter。");
    }
}
