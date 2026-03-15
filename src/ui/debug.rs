use eframe::egui;

use crate::app::{commands::AppCommand, state::AppState};

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    ui.heading("调试页");
    ui.label("当前保留最小调试信息，并开始承载 M3 的离线验证入口。");
    ui.separator();

    ui.label(format!("配置文件路径: {}", state.config_path));
    ui.label(format!(
        "当前页面: {}",
        state.settings.selected_page.label()
    ));
    ui.label(format!(
        "当前模式: {}",
        state.settings.inference_mode.label()
    ));

    if let Some(error) = &state.last_persist_error {
        ui.colored_label(
            egui::Color32::from_rgb(220, 90, 90),
            format!("最近一次配置写入失败: {error}"),
        );
    } else {
        ui.label("最近一次配置写入: 正常");
    }

    ui.separator();
    ui.heading("离线 Basic Filter 验证");
    ui.label(
        "当前使用默认 `speaker_profile.json` 对单个 WAV 做离线处理，并把结果输出为新的 WAV 文件。",
    );

    let default_profile_ready = state
        .default_profile_summary
        .as_ref()
        .is_some_and(|summary| !summary.is_metadata_only());

    if default_profile_ready {
        ui.colored_label(
            egui::Color32::from_rgb(70, 170, 90),
            "默认 profile 已就绪，可以直接运行离线 Basic Filter。",
        );
    } else {
        ui.colored_label(
            egui::Color32::from_rgb(220, 170, 90),
            "当前默认 profile 还不可用于离线 Basic Filter，请先完成训练并生成 embedding-ready profile。",
        );
    }

    ui.add_space(8.0);
    ui.label("输入 WAV 路径");
    ui.text_edit_singleline(&mut state.offline_basic_filter_input_path);
    ui.label("输出 WAV 路径");
    ui.text_edit_singleline(&mut state.offline_basic_filter_output_path);

    ui.horizontal(|ui| {
        if ui.button("恢复默认路径").clicked() {
            state.restore_default_offline_basic_filter_paths();
        }

        if ui
            .add_enabled(
                default_profile_ready,
                egui::Button::new("运行离线 Basic Filter"),
            )
            .clicked()
        {
            state.queue_command(AppCommand::RunOfflineBasicFilter);
        }
    });

    ui.small("默认输入是 `profiles/default/recordings/free_speech.wav`。");
    ui.small("默认输出是 `profiles/default/offline_outputs/free_speech_basic_filter.wav`。");

    if let Some(error) = &state.last_offline_basic_filter_error {
        ui.add_space(8.0);
        ui.colored_label(
            egui::Color32::from_rgb(220, 90, 90),
            format!("离线 Basic Filter 运行失败: {error}"),
        );
    }

    if let Some(metrics) = &state.last_offline_basic_filter_metrics {
        ui.add_space(8.0);
        ui.group(|ui| {
            ui.label("最近一次离线处理结果");
            ui.label(format!(
                "输入采样率: {} Hz | 输出采样率: {} Hz",
                metrics.input_sample_rate_hz, metrics.output_sample_rate_hz
            ));
            ui.label(format!(
                "输入时长: {:.2} s | 输出时长: {:.2} s",
                metrics.input_duration_seconds, metrics.output_duration_seconds
            ));
            ui.label(format!(
                "分析帧: {} | 活动帧: {} | 保留活动帧: {} | 抑制活动帧: {}",
                metrics.analyzed_frame_count,
                metrics.active_frame_count,
                metrics.kept_active_frame_count,
                metrics.suppressed_active_frame_count
            ));
            ui.label(format!(
                "相似度: 平均 {:.3} | 最低 {:.3} | 最高 {:.3} | 运行阈值 {:.3}",
                metrics.mean_similarity,
                metrics.min_similarity,
                metrics.max_similarity,
                metrics.operating_similarity_threshold
            ));
            ui.label(format!("平均帧增益: {:.3}", metrics.average_frame_gain));
            ui.label(format!(
                "输出文件: {}",
                state.offline_basic_filter_output_path
            ));
        });
    }

    ui.separator();
    ui.label("后续这里还会补日志视图、缓冲区状态、模型信息与诊断导出。");
}
