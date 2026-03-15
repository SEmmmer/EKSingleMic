use eframe::egui;

use crate::{
    app::{
        commands::AppCommand,
        state::{AppState, BusyAction},
    },
    audio::devices::DeviceDescriptor,
    config::settings::InferenceMode,
    pipeline::realtime::RuntimeStage,
};

const UNSELECTED_DEVICE_LABEL: &str = "未选择";

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    ui.heading("设备页");
    ui.label("M1 当前先实现 `cpal` 设备枚举、设备选择和基础状态展示。");
    ui.separator();

    ui.horizontal(|ui| {
        ui.label(format!("音频 Host: {}", state.device_inventory.host_name));
        if ui
            .add_enabled(!state.is_busy(), egui::Button::new("刷新设备"))
            .clicked()
        {
            state.queue_command(AppCommand::RefreshAudioDevices);
        }
    });

    if let Some(error) = &state.last_device_error {
        ui.colored_label(
            egui::Color32::from_rgb(220, 90, 90),
            format!("设备枚举失败: {error}"),
        );
    }

    if let Some(route) = state.device_inventory.virtual_cable_route.clone() {
        ui.group(|ui| {
            ui.label(format!(
                "已检测到 {}。程序输出应使用播放端：{}",
                route.driver_name, route.playback_device_name
            ));
            ui.label(format!(
                "OBS / Discord 应选择录音端：{}",
                route.recording_device_name
            ));

            let output_matches_route = state
                .settings
                .selected_output_device
                .as_deref()
                .is_some_and(|name| name == route.playback_device_name.as_str());

            if output_matches_route {
                ui.colored_label(
                    egui::Color32::from_rgb(90, 180, 120),
                    "当前程序输出已对准 VB-CABLE 播放端。",
                );
            } else {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 170, 90),
                    "当前输出设备不是 VB-CABLE，OBS / Discord 不会收到程序处理后的声音。",
                );
                if ui
                    .add_enabled(!state.is_busy(), egui::Button::new("切换输出到 VB-CABLE"))
                    .clicked()
                {
                    state.queue_command(AppCommand::UseDetectedVirtualCableOutput);
                }
            }
        });
        ui.separator();
    } else {
        ui.colored_label(
            egui::Color32::from_rgb(220, 170, 90),
            "未检测到 VB-CABLE 录音/播放端配对，程序暂时不会自动接管虚拟音频线。",
        );
        ui.separator();
    }

    ui.separator();
    let controls_enabled = !state.is_busy();
    render_device_selector(
        ui,
        "输入设备",
        &state.device_inventory.input_devices,
        &mut state.settings.selected_input_device,
        controls_enabled,
    );
    render_device_selector(
        ui,
        "输出设备",
        &state.device_inventory.output_devices,
        &mut state.settings.selected_output_device,
        controls_enabled,
    );

    ui.separator();
    if let Some(busy) = state.busy_state_for(BusyAction::StartRealtime) {
        show_loading_start_button(
            ui,
            state.settings.inference_mode,
            busy.detail.as_str(),
            busy.progress,
        );
    } else {
        match state.runtime_stage {
            RuntimeStage::Stopped => {
                if ui
                    .add_enabled(
                        !state.is_busy(),
                        egui::Button::new(start_button_label(state.settings.inference_mode)),
                    )
                    .clicked()
                {
                    state.begin_busy_action(
                        BusyAction::StartRealtime,
                        format!("正在准备启动 {}", state.settings.inference_mode.label()),
                        0.05,
                    );
                    state.queue_command(AppCommand::StartRealtime);
                }
            }
            RuntimeStage::RunningPassthrough | RuntimeStage::RunningBasicFilter => {
                if ui
                    .add_enabled(!state.is_busy(), egui::Button::new("停止实时链路"))
                    .clicked()
                {
                    state.queue_command(AppCommand::StopRealtime);
                }
            }
        }
    }

    ui.label(format!(
        "已发现 {} 个输入设备 / {} 个输出设备",
        state.device_inventory.input_devices.len(),
        state.device_inventory.output_devices.len()
    ));
    ui.label(format!("当前状态: {}", state.runtime_stage.label()));
    ui.label(format!(
        "输入峰值: {:.1} dBFS | 输出峰值: {:.1} dBFS",
        state.runtime_metrics.input_peak_dbfs, state.runtime_metrics.output_peak_dbfs
    ));
    ui.label(format!(
        "输入丢帧: {} | 输出补零帧: {}",
        state.runtime_metrics.dropped_input_frames, state.runtime_metrics.missing_output_frames
    ));
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
    }

    if let Some(format) = &state.runtime_format {
        ui.separator();
        ui.label(format!("输入设备: {}", format.input_device_name));
        ui.label(format!("输出设备: {}", format.output_device_name));
        ui.label(format!(
            "采样率: {} Hz | 输入: {} ch / {} | 输出: {} ch / {}",
            format.sample_rate_hz,
            format.input_channels,
            format.input_sample_format,
            format.output_channels,
            format.output_sample_format
        ));
    } else {
        ui.label("运行格式: 尚未启动实时链路。");
    }
}

fn start_button_label(mode: InferenceMode) -> &'static str {
    match mode {
        InferenceMode::Passthrough => "启动 Passthrough",
        InferenceMode::BasicFilter => "启动 Basic Filter",
        InferenceMode::StrongIsolation => "启动 Strong Isolation",
    }
}

fn show_loading_start_button(ui: &mut egui::Ui, mode: InferenceMode, detail: &str, progress: f32) {
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(16.0));
            ui.add_enabled(
                false,
                egui::Button::new(format!("正在启动 {}", mode.label())),
            );
        });
        ui.small(detail);
        ui.add(
            egui::ProgressBar::new(progress)
                .desired_width(280.0)
                .show_percentage(),
        );
    });
}

fn render_device_selector(
    ui: &mut egui::Ui,
    label: &str,
    devices: &[DeviceDescriptor],
    selected_device: &mut Option<String>,
    enabled: bool,
) {
    let mut selected_name = selected_device
        .clone()
        .unwrap_or_else(|| UNSELECTED_DEVICE_LABEL.to_owned());

    ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_label(label)
            .width(460.0)
            .selected_text(selected_name.clone())
            .show_ui(ui, |ui| {
                if devices.is_empty() {
                    ui.label("未发现可用设备");
                    return;
                }

                for device in devices {
                    ui.selectable_value(&mut selected_name, device.name.clone(), device.summary());
                }
            });
    });

    if selected_name == UNSELECTED_DEVICE_LABEL {
        *selected_device = None;
    } else {
        *selected_device = Some(selected_name);
    }
}
