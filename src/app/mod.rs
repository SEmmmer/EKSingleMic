pub mod commands;
pub mod state;

use std::{fs, path::PathBuf};

use anyhow::{Context, Result, anyhow};
use eframe::{CreationContext, egui};
use tracing_subscriber::EnvFilter;

use crate::{
    app::{commands::AppCommand, state::AppState},
    audio::devices::{DeviceInventory, enumerate_audio_devices},
    config::settings::SettingsStore,
    pipeline::realtime::{PassthroughRuntime, RuntimeStage},
    profile::record::{EnrollmentScript, RecordedClip, RecordingSession},
    profile::storage::{ProfileSummary, SpeakerProfileStore},
    ui,
};

pub const SOURCE_HAN_SANS_SC_BOLD_FAMILY: &str = "source_han_sans_sc_bold";

pub struct SingleMicApp {
    state: AppState,
    settings_store: SettingsStore,
    profile_store: SpeakerProfileStore,
    passthrough_runtime: Option<PassthroughRuntime>,
    training_recording_session: Option<RecordingSession>,
}

impl SingleMicApp {
    pub fn bootstrap(creation_context: &CreationContext<'_>) -> Result<Self> {
        install_source_han_sans(&creation_context.egui_ctx);

        let settings_store = SettingsStore::new()?;
        let profile_store = SpeakerProfileStore::new().context("failed to initialize speaker profile store")?;
        let mut settings = settings_store.load().context("failed to load settings")?;
        let config_path = settings_store.path().display().to_string();
        let (device_inventory, last_device_error) = Self::load_device_inventory();
        let enrollment_script =
            EnrollmentScript::load_bundled_zh_cn().context("failed to load bundled zh-CN prompts")?;
        let (default_profile_summary, last_profile_error) =
            Self::load_default_profile_summary(&profile_store);
        Self::prefer_detected_virtual_cable_output(&mut settings, &device_inventory);

        tracing::info!(config_path = %config_path, "application state loaded");

        Ok(Self {
            state: AppState::new(
                settings,
                config_path,
                device_inventory,
                last_device_error,
                enrollment_script,
                default_profile_summary,
                last_profile_error,
            ),
            settings_store,
            profile_store,
            passthrough_runtime: None,
            training_recording_session: None,
        })
    }

    fn load_device_inventory() -> (DeviceInventory, Option<String>) {
        match enumerate_audio_devices() {
            Ok(inventory) => (inventory, None),
            Err(error) => {
                let message = format!("{error:#}");
                tracing::warn!(error = %message, "failed to enumerate audio devices");
                (DeviceInventory::default(), Some(message))
            }
        }
    }

    fn load_default_profile_summary(
        profile_store: &SpeakerProfileStore,
    ) -> (Option<ProfileSummary>, Option<String>) {
        match profile_store.load_default_profile_summary() {
            Ok(summary) => (summary, None),
            Err(error) => {
                let message = format!("{error:#}");
                tracing::warn!(error = %message, "failed to inspect default profile");
                (None, Some(message))
            }
        }
    }

    fn persist_settings(&mut self) {
        match self.settings_store.save(&self.state.settings) {
            Ok(()) => {
                self.state.last_persist_error = None;
            }
            Err(error) => {
                let message = format!("{error:#}");
                tracing::error!(error = %message, "failed to persist settings");
                self.state.last_persist_error = Some(message);
            }
        }
    }

    fn handle_command(&mut self, command: AppCommand) {
        match command {
            AppCommand::RefreshAudioDevices => {
                let (inventory, error) = Self::load_device_inventory();
                self.state.device_inventory = inventory;
                self.state.last_device_error = error;
                Self::prefer_detected_virtual_cable_output(
                    &mut self.state.settings,
                    &self.state.device_inventory,
                );
            }
            AppCommand::UseDetectedVirtualCableOutput => self.use_detected_virtual_cable_output(),
            AppCommand::StartRealtime => self.start_passthrough(),
            AppCommand::StopRealtime => self.stop_passthrough(),
            AppCommand::AdvanceTrainingStep => self.advance_training_step(),
            AppCommand::RetryPreviousPrompt => self.retry_previous_prompt(),
            AppCommand::RestartTrainingFlow => self.restart_training_flow(),
        }
    }

    fn use_detected_virtual_cable_output(&mut self) {
        if let Some(route) = &self.state.device_inventory.virtual_cable_route {
            self.state.settings.selected_output_device = Some(route.playback_device_name.clone());
        }
    }

    fn start_passthrough(&mut self) {
        if self.training_recording_session.is_some() {
            self.state.last_runtime_error =
                Some("训练录音正在占用麦克风，请先完成或退出当前训练录音步骤。".to_owned());
            return;
        }

        self.stop_passthrough();

        match PassthroughRuntime::start(
            self.state.settings.selected_input_device.as_deref(),
            self.state.settings.selected_output_device.as_deref(),
        ) {
            Ok(runtime) => {
                self.state.runtime_format = Some(runtime.format_summary().clone());
                self.state.runtime_metrics = runtime.metrics_snapshot();
                self.state.runtime_stage = RuntimeStage::RunningPassthrough;
                self.state.last_runtime_error = None;
                self.passthrough_runtime = Some(runtime);
            }
            Err(error) => {
                let message = format!("{error:#}");
                tracing::error!(error = %message, "failed to start passthrough runtime");
                self.state.runtime_stage = RuntimeStage::Stopped;
                self.state.last_runtime_error = Some(message);
            }
        }
    }

    fn stop_passthrough(&mut self) {
        self.passthrough_runtime = None;
        self.state.runtime_stage = RuntimeStage::Stopped;
        self.state.runtime_metrics = Default::default();
        self.state.runtime_format = None;
    }

    fn sync_runtime_state(&mut self) {
        if let Some(runtime) = &self.passthrough_runtime {
            self.state.runtime_stage = RuntimeStage::RunningPassthrough;
            self.state.runtime_metrics = runtime.metrics_snapshot();
            self.state.runtime_format = Some(runtime.format_summary().clone());
        }
    }

    fn advance_training_step(&mut self) {
        if self.state.training_step.is_recording_phase() {
            self.finish_current_training_recording();
        }

        self.state.advance_training_step();
        self.start_training_recording_for_current_step();
    }

    fn retry_previous_prompt(&mut self) {
        if self.state.training_step.is_recording_phase() {
            self.discard_current_training_recording();
        }

        self.state.retry_previous_prompt();

        if let crate::app::state::TrainingStep::FixedPromptPreparation { index } = self.state.training_step {
            let removed = self.state.training_recordings.clear_from_prompt(index);
            self.delete_recorded_clips(removed);
        }
    }

    fn restart_training_flow(&mut self) {
        self.discard_current_training_recording();

        let removed = self.state.training_recordings.clear_all();
        self.delete_recorded_clips(removed);
        self.state.restart_training_flow();
        self.state.last_training_recording_error = None;
        self.reset_training_recording_feedback();
    }

    fn start_training_recording_for_current_step(&mut self) {
        let Some(kind) = self.state.training_step.recording_take_kind() else {
            self.reset_training_recording_feedback();
            return;
        };

        if self.training_recording_session.is_some() {
            return;
        }

        if self.passthrough_runtime.is_some() {
            self.state.last_training_recording_error =
                Some("训练录音开始前请先停止设备页中的 Passthrough。".to_owned());
            self.reset_training_recording_feedback();
            return;
        }

        match RecordingSession::start(kind, self.state.settings.selected_input_device.as_deref()) {
            Ok(session) => {
                self.training_recording_session = Some(session);
                self.state.last_training_recording_error = None;
                self.state.training_recording_active = true;
                self.state.training_input_level_linear = 0.0;
            }
            Err(error) => {
                let message = format!("{error:#}");
                tracing::error!(error = %message, "failed to start training recording");
                self.state.last_training_recording_error = Some(message);
                self.reset_training_recording_feedback();
            }
        }
    }

    fn finish_current_training_recording(&mut self) {
        let Some(session) = self.training_recording_session.take() else {
            self.reset_training_recording_feedback();
            return;
        };

        match session.finish() {
            Ok(clip) => {
                self.state.training_recordings.register(clip);
                self.state.last_training_recording_error = None;
            }
            Err(error) => {
                let message = format!("{error:#}");
                tracing::error!(error = %message, "failed to finalize training recording");
                self.state.last_training_recording_error = Some(message);
            }
        }

        self.reset_training_recording_feedback();
    }

    fn discard_current_training_recording(&mut self) {
        let Some(session) = self.training_recording_session.take() else {
            self.reset_training_recording_feedback();
            return;
        };

        if let Err(error) = session.discard() {
            let message = format!("{error:#}");
            tracing::error!(error = %message, "failed to discard training recording");
            self.state.last_training_recording_error = Some(message);
        }

        self.reset_training_recording_feedback();
    }

    fn sync_training_recording_feedback(&mut self) {
        if let Some(session) = &self.training_recording_session {
            let metrics = session.metrics_snapshot();
            self.state.training_recording_active = true;
            self.state.training_input_level_linear = metrics.input_level_linear;
        } else {
            self.reset_training_recording_feedback();
        }
    }

    fn reset_training_recording_feedback(&mut self) {
        self.state.training_recording_active = false;
        self.state.training_input_level_linear = 0.0;
    }

    fn delete_recorded_clips(&mut self, clips: Vec<RecordedClip>) {
        for clip in clips {
            let path = PathBuf::from(&clip.relative_path);
            if !path.exists() {
                continue;
            }

            if let Err(error) = fs::remove_file(&path) {
                let message = format!("failed to remove discarded recording {}: {error}", path.display());
                tracing::warn!(error = %message, "failed to delete discarded training recording");
                self.state.last_training_recording_error = Some(message);
            }
        }
    }

    fn prefer_detected_virtual_cable_output(
        settings: &mut crate::config::settings::AppSettings,
        inventory: &DeviceInventory,
    ) {
        if let Some(route) = &inventory.virtual_cable_route {
            settings.selected_output_device = Some(route.playback_device_name.clone());
        }
    }
}

impl eframe::App for SingleMicApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.sync_runtime_state();
        self.sync_training_recording_feedback();
        let previous_settings = self.state.settings.clone();

        egui::TopBottomPanel::top("app_top_bar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading("EKSingleMic");
                ui.separator();
                ui.label("M0 工程初始化");
                ui.separator();
                ui.label("本地 Rust + eframe/egui GUI 壳子");
            });
        });

        egui::SidePanel::left("app_sidebar")
            .resizable(false)
            .default_width(220.0)
            .show(ctx, |ui| ui::show_navigation(ui, &mut self.state));

        egui::TopBottomPanel::bottom("app_status_bar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("当前页面: {}", self.state.settings.selected_page.label()));
                ui.separator();
                ui.label(format!("推理模式: {}", self.state.settings.inference_mode.label()));
                ui.separator();
                ui.label(format!("音频状态: {}", self.state.runtime_stage.label()));
                ui.separator();
                ui.label(format!("配置文件: {}", self.state.config_path));
            });

            if let Some(error) = &self.state.last_persist_error {
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), format!("配置保存失败: {error}"));
            } else {
                ui.label("配置保存状态: 正常");
            }

            if let Some(error) = &self.state.last_runtime_error {
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), format!("实时链路错误: {error}"));
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| ui::show_page(ui, &mut self.state));

        if self.state.settings != previous_settings {
            self.persist_settings();
        }

        if let Some(command) = self.state.pending_command.take() {
            self.handle_command(command);
        }
    }
}

pub fn init_logging() -> Result<()> {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("ek_single_mic=info,wgpu=warn,naga=warn"));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .compact()
        .try_init()
        .map_err(|error| anyhow!("failed to install tracing subscriber: {error}"))?;

    Ok(())
}

fn install_source_han_sans(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let regular_font_name = "source_han_sans_sc_regular";
    let bold_font_name = SOURCE_HAN_SANS_SC_BOLD_FAMILY;

    fonts.font_data.clear();
    fonts.font_data.insert(
        regular_font_name.into(),
        egui::FontData::from_static(include_bytes!(
            "../../assets/fonts/SourceHanSansSC-Regular.otf"
        ))
        .into(),
    );
    fonts.font_data.insert(
        bold_font_name.into(),
        egui::FontData::from_static(include_bytes!(
            "../../assets/fonts/SourceHanSansSC-Bold.otf"
        ))
        .into(),
    );
    fonts
        .families
        .insert(egui::FontFamily::Proportional, vec![regular_font_name.into()]);
    fonts
        .families
        .insert(egui::FontFamily::Monospace, vec![regular_font_name.into()]);
    fonts.families.insert(
        egui::FontFamily::Name(bold_font_name.into()),
        vec![bold_font_name.into()],
    );

    ctx.set_fonts(fonts);
    tracing::info!("installed bundled Source Han Sans SC regular and bold fonts");
}
