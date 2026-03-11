use eframe::egui;

use crate::app::state::AppState;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    ui.heading("调试页");
    ui.label("M0 先展示最小可见调试信息。");
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
        ui.colored_label(egui::Color32::from_rgb(220, 90, 90), format!("最近一次配置写入失败: {error}"));
    } else {
        ui.label("最近一次配置写入: 正常");
    }

    ui.separator();
    ui.label("后续这里将补充日志视图、缓冲区状态、模型信息与诊断导出。");
}
