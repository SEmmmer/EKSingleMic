pub mod commands;
pub mod state;

use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
};

use anyhow::{Context, Result, anyhow};
use eframe::{CreationContext, egui};
use tracing_subscriber::EnvFilter;

use crate::{
    app::{
        commands::AppCommand,
        state::{AppState, BusyAction},
    },
    audio::devices::{DeviceInventory, enumerate_audio_devices},
    config::settings::UiPage,
    config::settings::{InferenceMode, SettingsStore},
    pipeline::{
        OfflineBasicFilterProcessor,
        realtime::{RealtimeRuntime, RuntimeStage},
    },
    profile::storage::{ProfileSummary, SpeakerProfileStore},
    profile::{
        build::SpeakerProfileBuilder,
        quality::QualityReport,
        record::{
            EnrollmentScript, RecordedClip, RecordingPreviewSession, RecordingSession,
            clear_default_recordings_dir, scan_default_recordings, source_recordings_from_manifest,
        },
    },
    ui,
};

pub const SOURCE_HAN_SANS_SC_BOLD_FAMILY: &str = "source_han_sans_sc_bold";

pub struct SingleMicApp {
    state: AppState,
    settings_store: SettingsStore,
    profile_store: SpeakerProfileStore,
    realtime_runtime: Option<RealtimeRuntime>,
    training_recording_session: Option<RecordingSession>,
    training_preview_session: Option<RecordingPreviewSession>,
    background_task: Option<BackgroundTaskHandle>,
}

struct BackgroundTaskHandle {
    action: BusyAction,
    receiver: Receiver<BackgroundTaskEvent>,
    pending_result: Option<BackgroundTaskResult>,
}

enum BackgroundTaskEvent {
    Progress { detail: String, progress: f32 },
    Finished(BackgroundTaskResult),
}

enum BackgroundTaskResult {
    LoadDetectedTrainingRecordings(LoadDetectedTrainingRecordingsTaskResult),
    StartRealtime(StartRealtimeTaskResult),
}

struct LoadDetectedTrainingRecordingsTaskResult {
    manifest: crate::profile::record::TrainingRecordingManifest,
    quality_report: Option<QualityReport>,
    quality_error: Option<String>,
    profile_refresh: ProfileRefreshOutcome,
}

enum ProfileRefreshOutcome {
    Unchanged,
    Updated(Option<ProfileSummary>),
    Failed(String),
}

struct StartRealtimeTaskResult {
    runtime: Result<RealtimeRuntime, String>,
}

impl SingleMicApp {
    pub fn bootstrap(creation_context: &CreationContext<'_>) -> Result<Self> {
        install_source_han_sans(&creation_context.egui_ctx);

        let settings_store = SettingsStore::new()?;
        let profile_store =
            SpeakerProfileStore::new().context("failed to initialize speaker profile store")?;
        let mut settings = settings_store.load().context("failed to load settings")?;
        let config_path = settings_store.path().display().to_string();
        let (device_inventory, last_device_error) = Self::load_device_inventory();
        let enrollment_script = EnrollmentScript::load_bundled_zh_cn()
            .context("failed to load bundled zh-CN prompts")?;
        let (default_profile_summary, last_profile_error) =
            Self::load_default_profile_summary(&profile_store);
        let startup_recording_prompt =
            Self::inspect_startup_recording_prompt(&profile_store, enrollment_script.prompts.len());
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
                startup_recording_prompt,
            ),
            settings_store,
            profile_store,
            realtime_runtime: None,
            training_recording_session: None,
            training_preview_session: None,
            background_task: None,
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
            AppCommand::StartRealtime => self.start_realtime_runtime_task(),
            AppCommand::StopRealtime => self.stop_realtime_runtime(),
            AppCommand::RunOfflineBasicFilter => self.run_offline_basic_filter(),
            AppCommand::AdvanceTrainingStep => self.advance_training_step(),
            AppCommand::RetryPreviousPrompt => self.retry_previous_prompt(),
            AppCommand::RestartTrainingFlow => self.restart_training_flow(),
            AppCommand::StartReviewRerecord { kind } => self.start_review_rerecord(kind),
            AppCommand::PreviewRecordedClip { kind } => self.start_preview_recording(kind),
            AppCommand::LoadDetectedTrainingRecordings => {
                self.start_load_detected_training_recordings_task()
            }
            AppCommand::OverwriteDetectedTrainingRecordings => {
                self.overwrite_detected_training_recordings()
            }
        }
    }

    fn use_detected_virtual_cable_output(&mut self) {
        if let Some(route) = &self.state.device_inventory.virtual_cable_route {
            self.state.settings.selected_output_device = Some(route.playback_device_name.clone());
        }
    }

    fn start_realtime_runtime_task(&mut self) {
        if self.training_recording_session.is_some() {
            self.state.last_runtime_error =
                Some("训练录音正在占用麦克风，请先完成或退出当前训练录音步骤。".to_owned());
            self.state.finish_busy_action();
            return;
        }

        if self.background_task.is_some() {
            return;
        }

        self.stop_training_preview();
        self.stop_realtime_runtime();

        let mode = self.state.settings.inference_mode;
        let selected_input_device = self.state.settings.selected_input_device.clone();
        let selected_output_device = self.state.settings.selected_output_device.clone();
        let profile_store = self.profile_store.clone();
        let (sender, receiver) = mpsc::channel();
        let spawn_result = spawn_background_task("ek-start-realtime-task", move || {
            send_task_progress(&sender, "正在准备实时链路", 0.08);

            let loaded_profile = if matches!(mode, InferenceMode::BasicFilter) {
                send_task_progress(&sender, "正在加载默认 speaker profile", 0.28);
                match profile_store.load_default() {
                    Ok(profile) => Some(profile),
                    Err(error) => {
                        let message = format!("{error:#}");
                        tracing::error!(
                            error = %message,
                            "failed to load default profile for realtime basic filter"
                        );
                        let _ = sender.send(BackgroundTaskEvent::Finished(
                            BackgroundTaskResult::StartRealtime(StartRealtimeTaskResult {
                                runtime: Err(message),
                            }),
                        ));
                        return;
                    }
                }
            } else {
                None
            };

            send_task_progress(&sender, "正在打开音频设备并创建实时链路", 0.62);
            let runtime = RealtimeRuntime::start(
                mode,
                selected_input_device.as_deref(),
                selected_output_device.as_deref(),
                loaded_profile.as_ref(),
            )
            .map_err(|error| {
                let message = format!("{error:#}");
                tracing::error!(
                    error = %message,
                    mode = %mode.label(),
                    "failed to start realtime runtime"
                );
                message
            });

            send_task_progress(&sender, "正在同步实时状态", 0.9);
            let _ = sender.send(BackgroundTaskEvent::Finished(
                BackgroundTaskResult::StartRealtime(StartRealtimeTaskResult { runtime }),
            ));
        });

        match spawn_result {
            Ok(()) => {
                self.background_task = Some(BackgroundTaskHandle {
                    action: BusyAction::StartRealtime,
                    receiver,
                    pending_result: None,
                });
            }
            Err(error) => {
                self.state.finish_busy_action();
                self.state.runtime_stage = RuntimeStage::Stopped;
                self.state.last_runtime_error = Some(format!("无法启动实时后台任务：{error}"));
            }
        }
    }

    fn stop_realtime_runtime(&mut self) {
        self.realtime_runtime = None;
        self.state.runtime_stage = RuntimeStage::Stopped;
        self.state.runtime_metrics = Default::default();
        self.state.runtime_format = None;
    }

    fn sync_runtime_state(&mut self) {
        if let Some(runtime) = &self.realtime_runtime {
            self.state.runtime_stage = runtime.stage();
            self.state.runtime_metrics = runtime.metrics_snapshot();
            self.state.runtime_format = Some(runtime.format_summary().clone());
        }
    }

    fn poll_runtime_output_readiness(&mut self) {
        let Some(runtime) = &self.realtime_runtime else {
            return;
        };
        if self.background_task.is_some()
            || self
                .state
                .busy_state_for(BusyAction::StartRealtime)
                .is_none()
        {
            return;
        }

        let output_name = runtime.format_summary().output_device_name.clone();
        if runtime.is_output_ready()
            && self
                .state
                .has_satisfied_busy_minimum_duration(BusyAction::StartRealtime)
        {
            self.state.finish_busy_action();
            return;
        }

        self.state.update_busy_action(
            BusyAction::StartRealtime,
            format!("正在等待 {output_name} 进入可输出 Basic Filter 音频状态"),
            0.98,
        );
    }

    fn poll_background_task(&mut self) {
        enum PollOutcome {
            Finished(BackgroundTaskResult),
            Disconnected(BusyAction),
        }

        let Some(mut task) = self.background_task.take() else {
            return;
        };

        if let Some(result) = task.pending_result.take() {
            if self.state.has_satisfied_busy_minimum_duration(task.action) {
                if self.apply_background_task_result(result) {
                    self.state.finish_busy_action();
                }
            } else {
                task.pending_result = Some(result);
                self.background_task = Some(task);
            }
            return;
        }

        let mut outcome = None;

        loop {
            match task.receiver.try_recv() {
                Ok(BackgroundTaskEvent::Progress { detail, progress }) => {
                    self.state.update_busy_action(task.action, detail, progress);
                }
                Ok(BackgroundTaskEvent::Finished(result)) => {
                    outcome = Some(PollOutcome::Finished(result));
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    outcome = Some(PollOutcome::Disconnected(task.action));
                    break;
                }
            }
        }

        match outcome {
            Some(PollOutcome::Finished(result)) => {
                if self.state.has_satisfied_busy_minimum_duration(task.action) {
                    if self.apply_background_task_result(result) {
                        self.state.finish_busy_action();
                    }
                } else {
                    task.pending_result = Some(result);
                    self.background_task = Some(task);
                }
            }
            Some(PollOutcome::Disconnected(action)) => {
                self.state.finish_busy_action();
                self.apply_background_task_disconnect(action);
            }
            None => {
                self.background_task = Some(task);
            }
        }
    }

    fn apply_background_task_result(&mut self, result: BackgroundTaskResult) -> bool {
        match result {
            BackgroundTaskResult::LoadDetectedTrainingRecordings(result) => {
                self.apply_load_detected_training_recordings_result(result);
                true
            }
            BackgroundTaskResult::StartRealtime(result) => {
                self.apply_start_realtime_task_result(result)
            }
        }
    }

    fn apply_background_task_disconnect(&mut self, action: BusyAction) {
        let message = "后台任务意外中断，未能返回结果。".to_owned();
        tracing::error!(action = ?action, error = %message, "background task disconnected");

        match action {
            BusyAction::LoadDetectedTrainingRecordings => {
                self.state.last_training_quality_error = Some(message);
            }
            BusyAction::StartRealtime => {
                self.state.runtime_stage = RuntimeStage::Stopped;
                self.state.last_runtime_error = Some(message);
            }
        }
    }

    fn apply_load_detected_training_recordings_result(
        &mut self,
        result: LoadDetectedTrainingRecordingsTaskResult,
    ) {
        self.state
            .load_detected_training_recordings(result.manifest);
        self.state.training_quality_report = result.quality_report;
        self.state.last_training_quality_error = result.quality_error;

        match result.profile_refresh {
            ProfileRefreshOutcome::Unchanged => {}
            ProfileRefreshOutcome::Updated(summary) => {
                self.state.default_profile_summary = summary;
                self.state.last_profile_error = None;
            }
            ProfileRefreshOutcome::Failed(message) => {
                self.state.last_profile_error = Some(message);
            }
        }
    }

    fn apply_start_realtime_task_result(&mut self, result: StartRealtimeTaskResult) -> bool {
        match result.runtime {
            Ok(runtime) => {
                self.state.runtime_stage = runtime.stage();
                self.state.runtime_format = Some(runtime.format_summary().clone());
                self.state.runtime_metrics = runtime.metrics_snapshot();
                self.state.last_runtime_error = None;
                let output_name = runtime.format_summary().output_device_name.clone();
                let output_ready = runtime.is_output_ready();
                self.realtime_runtime = Some(runtime);
                if output_ready {
                    true
                } else {
                    self.state.update_busy_action(
                        BusyAction::StartRealtime,
                        format!("正在等待 {output_name} 进入可输出 Basic Filter 音频状态"),
                        0.98,
                    );
                    false
                }
            }
            Err(message) => {
                self.state.runtime_stage = RuntimeStage::Stopped;
                self.state.runtime_format = None;
                self.state.runtime_metrics = Default::default();
                self.state.last_runtime_error = Some(message);
                true
            }
        }
    }

    fn advance_training_step(&mut self) {
        if self.state.training_step.is_recording_phase() {
            self.finish_current_training_recording();
        }

        self.state.advance_training_step();
        self.refresh_training_quality_report();
        self.start_training_recording_for_current_step();
    }

    fn retry_previous_prompt(&mut self) {
        if self.state.training_step.is_recording_phase() {
            self.discard_current_training_recording();
        }

        self.state.retry_previous_prompt();

        if let crate::app::state::TrainingStep::FixedPromptPreparation { index } =
            self.state.training_step
        {
            let removed = self.state.training_recordings.clear_from_prompt(index);
            self.delete_recorded_clips(removed);
        }

        self.clear_training_quality_report();
    }

    fn restart_training_flow(&mut self) {
        self.stop_training_preview();
        self.discard_current_training_recording();

        let removed = self.state.training_recordings.clear_all();
        self.delete_recorded_clips(removed);
        self.state.restart_training_flow();
        self.state.last_training_recording_error = None;
        self.reset_training_recording_feedback();
        self.clear_training_quality_report();
    }

    fn start_load_detected_training_recordings_task(&mut self) {
        let Some(prompt) = self.state.startup_recording_prompt.clone() else {
            self.state.finish_busy_action();
            return;
        };

        if self.background_task.is_some() {
            return;
        }

        self.stop_training_preview();
        self.discard_current_training_recording();
        self.reset_training_recording_feedback();
        self.state.last_training_recording_error = None;
        self.state.last_training_preview_error = None;
        self.state.last_training_quality_error = None;

        let manifest = prompt.detected_recordings.manifest;
        let enrollment_script = self.state.enrollment_script.clone();
        let profile_store = self.profile_store.clone();
        let (sender, receiver) = mpsc::channel();
        let spawn_result = spawn_background_task("ek-load-recordings-task", move || {
            let total_clips = manifest.recorded_clip_count();
            let total_clips = total_clips.max(1);
            send_task_progress(&sender, format!("正在载入训练音频 0/{total_clips}"), 0.02);

            let quality_report = match QualityReport::analyze_manifest_with_progress(
                &manifest,
                |done, total, clip| {
                    let progress = 0.08 + 0.72 * (done as f32 / total.max(1) as f32);
                    send_task_progress(
                        &sender,
                        format!("正在载入训练音频 {done}/{total}：{}", clip.label),
                        progress,
                    );
                },
            ) {
                Ok(report) => Some(report),
                Err(error) => {
                    let message = format!("{error:#}");
                    tracing::error!(error = %message, "failed to analyze training quality");
                    let _ = sender.send(BackgroundTaskEvent::Finished(
                        BackgroundTaskResult::LoadDetectedTrainingRecordings(
                            LoadDetectedTrainingRecordingsTaskResult {
                                manifest,
                                quality_report: None,
                                quality_error: Some(message),
                                profile_refresh: ProfileRefreshOutcome::Unchanged,
                            },
                        ),
                    ));
                    return;
                }
            };

            send_task_progress(
                &sender,
                format!("训练音频已载入 {total_clips}/{total_clips}，正在刷新默认 profile"),
                0.86,
            );
            let profile_refresh = match quality_report.as_ref() {
                Some(report) => match SpeakerProfileBuilder::build_default(
                    &manifest,
                    report,
                    &enrollment_script,
                ) {
                    Ok(profile) => {
                        if let Err(error) = profile_store.save_default(&profile) {
                            let message = format!("{error:#}");
                            tracing::error!(error = %message, "failed to save default profile");
                            ProfileRefreshOutcome::Failed(message)
                        } else {
                            send_task_progress(
                                &sender,
                                format!(
                                    "训练音频已载入 {total_clips}/{total_clips}，正在同步 profile 摘要"
                                ),
                                0.95,
                            );
                            match profile_store.load_default_profile_summary() {
                                Ok(summary) => ProfileRefreshOutcome::Updated(summary),
                                Err(error) => {
                                    let message = format!("{error:#}");
                                    tracing::error!(
                                        error = %message,
                                        "failed to inspect default profile after async reload"
                                    );
                                    ProfileRefreshOutcome::Failed(message)
                                }
                            }
                        }
                    }
                    Err(error) => {
                        let message = format!("{error:#}");
                        tracing::error!(
                            error = %message,
                            "failed to build default profile from training data"
                        );
                        ProfileRefreshOutcome::Failed(message)
                    }
                },
                None => ProfileRefreshOutcome::Unchanged,
            };

            let _ = sender.send(BackgroundTaskEvent::Finished(
                BackgroundTaskResult::LoadDetectedTrainingRecordings(
                    LoadDetectedTrainingRecordingsTaskResult {
                        manifest,
                        quality_report,
                        quality_error: None,
                        profile_refresh,
                    },
                ),
            ));
        });

        match spawn_result {
            Ok(()) => {
                self.background_task = Some(BackgroundTaskHandle {
                    action: BusyAction::LoadDetectedTrainingRecordings,
                    receiver,
                    pending_result: None,
                });
            }
            Err(error) => {
                self.state.finish_busy_action();
                self.state.last_training_quality_error =
                    Some(format!("无法启动录音加载后台任务：{error}"));
            }
        }
    }

    fn overwrite_detected_training_recordings(&mut self) {
        self.stop_training_preview();
        self.discard_current_training_recording();

        if let Err(error) = clear_default_recordings_dir() {
            let message = format!("{error:#}");
            tracing::error!(error = %message, "failed to clear default recordings directory");
            self.state.last_training_recording_error = Some(message);
            return;
        }

        self.restart_training_flow();
        self.state.settings.selected_page = UiPage::Training;
        self.state.dismiss_startup_recording_prompt();
    }

    fn start_review_rerecord(&mut self, kind: crate::profile::record::RecordingTakeKind) {
        self.stop_training_preview();
        self.state.begin_review_rerecord(kind);
        self.clear_training_quality_report();
        self.state.last_training_recording_error = None;
        self.start_training_recording_for_current_step();
    }

    fn start_preview_recording(&mut self, kind: crate::profile::record::RecordingTakeKind) {
        if self.training_recording_session.is_some() {
            self.state.last_training_preview_error =
                Some("当前正在录音，暂时不能预览录音片段。".to_owned());
            return;
        }

        if self.realtime_runtime.is_some() {
            self.state.last_training_preview_error =
                Some("预览录音前请先停止当前实时链路。".to_owned());
            return;
        }

        let Some(clip) = self.state.training_recordings.get(kind).cloned() else {
            self.state.last_training_preview_error =
                Some("当前录音片段还不存在，无法预览。".to_owned());
            return;
        };

        self.stop_training_preview();

        match RecordingPreviewSession::start(&clip) {
            Ok(session) => {
                self.training_preview_session = Some(session);
                self.state.previewing_recording_kind = Some(kind);
                self.state.last_training_preview_error = None;
            }
            Err(error) => {
                let message = format!("{error:#}");
                tracing::error!(error = %message, "failed to start recording preview");
                self.state.previewing_recording_kind = None;
                self.state.last_training_preview_error = Some(message);
            }
        }
    }

    fn start_training_recording_for_current_step(&mut self) {
        let Some(kind) = self.state.training_step.recording_take_kind() else {
            self.reset_training_recording_feedback();
            return;
        };

        if self.training_recording_session.is_some() {
            return;
        }

        if self.realtime_runtime.is_some() {
            self.state.last_training_recording_error =
                Some("训练录音开始前请先停止当前实时链路。".to_owned());
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

    fn run_offline_basic_filter(&mut self) {
        let input_path = self.state.offline_basic_filter_input_path.trim();
        let output_path = self.state.offline_basic_filter_output_path.trim();

        if input_path.is_empty() || output_path.is_empty() {
            self.state.last_offline_basic_filter_metrics = None;
            self.state.last_offline_basic_filter_error =
                Some("离线输入 WAV 路径和输出 WAV 路径都不能为空。".to_owned());
            return;
        }

        if normalized_path_key(input_path) == normalized_path_key(output_path) {
            self.state.last_offline_basic_filter_metrics = None;
            self.state.last_offline_basic_filter_error =
                Some("离线输入 WAV 路径和输出 WAV 路径不能相同。".to_owned());
            return;
        }

        let processor = OfflineBasicFilterProcessor::default();
        match processor
            .process_default_profile_wav(&PathBuf::from(input_path), &PathBuf::from(output_path))
        {
            Ok(metrics) => {
                self.state.last_offline_basic_filter_metrics = Some(metrics);
                self.state.last_offline_basic_filter_error = None;
            }
            Err(error) => {
                let message = format!("{error:#}");
                tracing::error!(error = %message, input_path, output_path, "failed to run offline basic filter");
                self.state.last_offline_basic_filter_metrics = None;
                self.state.last_offline_basic_filter_error = Some(message);
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

    fn sync_training_preview_state(&mut self) {
        let Some(session) = &self.training_preview_session else {
            self.state.previewing_recording_kind = None;
            return;
        };

        if session.is_finished() {
            self.stop_training_preview();
        }
    }

    fn stop_training_preview(&mut self) {
        self.training_preview_session = None;
        self.state.previewing_recording_kind = None;
    }

    fn refresh_training_quality_report(&mut self) {
        if !matches!(
            self.state.training_step,
            crate::app::state::TrainingStep::Review
        ) {
            self.clear_training_quality_report();
            return;
        }

        match QualityReport::analyze_manifest(&self.state.training_recordings) {
            Ok(report) => {
                self.refresh_default_profile_from_training(&report);
                self.state.training_quality_report = Some(report);
                self.state.last_training_quality_error = None;
            }
            Err(error) => {
                let message = format!("{error:#}");
                tracing::error!(error = %message, "failed to analyze training quality");
                self.state.training_quality_report = None;
                self.state.last_training_quality_error = Some(message);
            }
        }
    }

    fn clear_training_quality_report(&mut self) {
        self.state.training_quality_report = None;
        self.state.last_training_quality_error = None;
    }

    fn refresh_default_profile_from_training(&mut self, report: &QualityReport) {
        let profile = match SpeakerProfileBuilder::build_default(
            &self.state.training_recordings,
            report,
            &self.state.enrollment_script,
        ) {
            Ok(profile) => profile,
            Err(error) => {
                let message = format!("{error:#}");
                tracing::error!(error = %message, "failed to build default profile from training data");
                self.state.last_profile_error = Some(message);
                return;
            }
        };

        if let Err(error) = self.profile_store.save_default(&profile) {
            let message = format!("{error:#}");
            tracing::error!(error = %message, "failed to save default profile");
            self.state.last_profile_error = Some(message);
            return;
        }

        let (summary, error) = Self::load_default_profile_summary(&self.profile_store);
        self.state.default_profile_summary = summary;
        self.state.last_profile_error = error;
    }

    fn delete_recorded_clips(&mut self, clips: Vec<RecordedClip>) {
        for clip in clips {
            let path = PathBuf::from(&clip.relative_path);
            if !path.exists() {
                continue;
            }

            if let Err(error) = fs::remove_file(&path) {
                let message = format!(
                    "failed to remove discarded recording {}: {error}",
                    path.display()
                );
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

    fn inspect_startup_recording_prompt(
        profile_store: &SpeakerProfileStore,
        prompt_count: usize,
    ) -> Option<crate::app::state::StartupRecordingPrompt> {
        let detected = match scan_default_recordings(prompt_count) {
            Ok(Some(detected)) => detected,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "failed to inspect startup recordings");
                return None;
            }
        };

        let mut details = Vec::new();
        let expected_total = prompt_count + 2;
        details.push(format!(
            "已识别有效训练录音 {}/{} 个。",
            detected.recognized_count(),
            expected_total
        ));

        if detected.is_complete() {
            details.push("环境静音、10 句固定短句和自由发挥录音都已检测到。".to_owned());
        } else {
            details.push("当前训练录音并不完整。".to_owned());
            for path in &detected.missing_paths {
                details.push(format!("缺少：{path}"));
            }
        }

        for entry in &detected.unexpected_entries {
            details.push(format!("杂项：{entry}"));
        }

        for entry in &detected.invalid_entries {
            details.push(format!("不可读取：{entry}"));
        }

        let detected_sources = source_recordings_from_manifest(&detected.manifest);
        let profile_match = if profile_store.default_profile_path().exists() {
            match profile_store.load_default() {
                Ok(profile) => {
                    let matches = normalized_path_set(&profile.source_recordings)
                        == normalized_path_set(&detected_sources)
                        && detected.is_complete()
                        && detected.unexpected_entries.is_empty()
                        && detected.invalid_entries.is_empty();
                    if !matches {
                        details.push(
                            "当前默认 speaker_profile.json 与录音文件没有一一对应。".to_owned(),
                        );
                    }
                    matches
                }
                Err(error) => {
                    details.push(format!(
                        "当前默认 speaker_profile.json 读取失败，无法完成一一对应校验：{error:#}"
                    ));
                    false
                }
            }
        } else {
            details.push("当前默认 speaker_profile.json 不存在，无法完成一一对应校验。".to_owned());
            false
        };

        let clean_complete = detected.is_complete()
            && detected.unexpected_entries.is_empty()
            && detected.invalid_entries.is_empty();

        let (severity, title, summary) = if clean_complete && profile_match {
            (
                crate::app::state::StartupRecordingPromptSeverity::Info,
                "检测到之前保存的录音".to_owned(),
                "当前录音文件齐全，并且与 `profiles/default/speaker_profile.json` 一一对应。"
                    .to_owned(),
            )
        } else {
            (
                crate::app::state::StartupRecordingPromptSeverity::Warning,
                "检测到旧训练录音，但目录状态需要注意".to_owned(),
                "当前录音文件不完整、存在杂项，或与默认 profile 不一一对应。你仍可选择加载文件或全部覆盖重录。".to_owned(),
            )
        };

        Some(crate::app::state::StartupRecordingPrompt {
            severity,
            title,
            summary,
            details,
            detected_recordings: detected,
        })
    }
}

impl eframe::App for SingleMicApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.sync_runtime_state();
        self.sync_training_recording_feedback();
        self.sync_training_preview_state();
        self.poll_background_task();
        self.sync_runtime_state();
        self.poll_runtime_output_readiness();

        if self.training_preview_session.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }

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
                ui.label(format!(
                    "当前页面: {}",
                    self.state.settings.selected_page.label()
                ));
                ui.separator();
                ui.label(format!(
                    "推理模式: {}",
                    self.state.settings.inference_mode.label()
                ));
                ui.separator();
                ui.label(format!("音频状态: {}", self.state.runtime_stage.label()));
                ui.separator();
                ui.label(format!("配置文件: {}", self.state.config_path));
            });

            if let Some(error) = &self.state.last_persist_error {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 90, 90),
                    format!("配置保存失败: {error}"),
                );
            } else {
                ui.label("配置保存状态: 正常");
            }

            if let Some(error) = &self.state.last_runtime_error {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 90, 90),
                    format!("实时链路错误: {error}"),
                );
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| ui::show_page(ui, &mut self.state));
        show_startup_recording_prompt(ctx, &mut self.state);

        if self.state.settings != previous_settings {
            self.persist_settings();
        }

        if self.background_task.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }

        if let Some(command) = self.state.pending_command.take() {
            self.handle_command(command);
            if self.state.is_busy() || self.background_task.is_some() {
                ctx.request_repaint();
            }
        }
    }
}

fn spawn_background_task(name: &str, task: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(task)
        .map(|_| ())
}

fn send_task_progress(
    sender: &Sender<BackgroundTaskEvent>,
    detail: impl Into<String>,
    progress: f32,
) {
    let _ = sender.send(BackgroundTaskEvent::Progress {
        detail: detail.into(),
        progress: progress.clamp(0.0, 1.0),
    });
}

fn normalized_path_set(paths: &[String]) -> BTreeSet<String> {
    paths.iter().map(|path| normalized_path_key(path)).collect()
}

fn normalized_path_key(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

fn show_startup_recording_prompt(ctx: &egui::Context, state: &mut AppState) {
    let Some(prompt) = state.startup_recording_prompt.clone() else {
        return;
    };

    let summary_color = match prompt.severity {
        crate::app::state::StartupRecordingPromptSeverity::Info => {
            egui::Color32::from_rgb(70, 170, 90)
        }
        crate::app::state::StartupRecordingPromptSeverity::Warning => {
            egui::Color32::from_rgb(220, 170, 90)
        }
    };

    egui::Window::new(prompt.title)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .collapsible(false)
        .resizable(false)
        .default_width(560.0)
        .show(ctx, |ui| {
            ui.colored_label(summary_color, prompt.summary);
            ui.add_space(8.0);

            for detail in &prompt.details {
                ui.label(detail);
            }

            ui.separator();
            ui.label("加载文件后会直接跳到第 25 步：完成确认，并立即触发一次质量检查。");

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        prompt.detected_recordings.can_load() && !state.is_busy(),
                        egui::Button::new("加载文件"),
                    )
                    .clicked()
                {
                    state.cancel_startup_recording_overwrite_confirmation();
                    let total_clips = prompt.detected_recordings.recognized_count().max(1);
                    state.begin_busy_action(
                        BusyAction::LoadDetectedTrainingRecordings,
                        format!("正在载入训练音频 0/{total_clips}"),
                        0.02,
                    );
                    state.queue_command(AppCommand::LoadDetectedTrainingRecordings);
                }

                if ui
                    .add_enabled(!state.is_busy(), egui::Button::new("全部覆盖重录"))
                    .clicked()
                    && state.confirm_startup_recording_overwrite()
                {
                    state.queue_command(AppCommand::OverwriteDetectedTrainingRecordings);
                }
            });

            if !prompt.detected_recordings.can_load() {
                ui.small("当前没有可加载的有效训练录音文件，只能选择全部覆盖重录。");
            }

            if let Some(busy) = state.busy_state_for(BusyAction::LoadDetectedTrainingRecordings) {
                ui.add_space(8.0);
                ui.label(&busy.detail);
                ui.add(
                    egui::ProgressBar::new(busy.progress)
                        .desired_width(520.0)
                        .show_percentage()
                        .text(&busy.detail),
                );
            }

            if let Some((clicks, required)) = state.startup_recording_overwrite_progress() {
                ui.small(format!(
                    "“全部覆盖重录”需要连续点击 {required} 次才会触发：{clicks}/{required}"
                ));
            } else {
                ui.small(format!(
                    "“全部覆盖重录”需要连续点击 {} 次才会真正触发。",
                    crate::app::state::STARTUP_RECORDING_OVERWRITE_CONFIRMATION_CLICKS
                ));
            }
        });
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
    fonts.families.insert(
        egui::FontFamily::Proportional,
        vec![regular_font_name.into()],
    );
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
