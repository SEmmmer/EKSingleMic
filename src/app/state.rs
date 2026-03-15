use std::time::{Duration, Instant};

use crate::{
    app::commands::AppCommand,
    audio::devices::DeviceInventory,
    config::settings::{AppSettings, UiPage},
    pipeline::{
        OfflineBasicFilterMetrics,
        realtime::{RuntimeFormatSummary, RuntimeMetricsSnapshot, RuntimeStage},
    },
    profile::{
        quality::QualityReport,
        record::{
            DetectedTrainingRecordings, EnrollmentScript, RecordingTakeKind,
            TrainingRecordingManifest,
        },
        storage::{DEFAULT_PROFILE_ID, ProfileSummary},
    },
};

pub const RESTART_TRAINING_CONFIRMATION_CLICKS: u8 = 3;
pub const RETRY_PREVIOUS_PROMPT_CONFIRMATION_CLICKS: u8 = 3;
pub const REVIEW_SUMMARY_RERECORD_CONFIRMATION_CLICKS: u8 = 3;
pub const STARTUP_RECORDING_OVERWRITE_CONFIRMATION_CLICKS: u8 = 3;

fn default_offline_basic_filter_input_path() -> String {
    format!("profiles/{DEFAULT_PROFILE_ID}/recordings/free_speech.wav")
}

fn default_offline_basic_filter_output_path() -> String {
    format!("profiles/{DEFAULT_PROFILE_ID}/offline_outputs/free_speech_basic_filter.wav")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupRecordingPromptSeverity {
    Info,
    Warning,
}

#[derive(Debug, Clone)]
pub struct StartupRecordingPrompt {
    pub severity: StartupRecordingPromptSeverity,
    pub title: String,
    pub summary: String,
    pub details: Vec<String>,
    pub detected_recordings: DetectedTrainingRecordings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusyAction {
    LoadDetectedTrainingRecordings,
    StartRealtime,
}

impl BusyAction {
    pub fn minimum_visible_duration(self) -> Duration {
        match self {
            Self::LoadDetectedTrainingRecordings => Duration::from_millis(0),
            Self::StartRealtime => Duration::from_millis(450),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BusyState {
    pub action: BusyAction,
    pub detail: String,
    pub progress: f32,
    pub started_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainingStep {
    Preparation,
    AmbientSilence,
    FixedPromptPreparation { index: usize },
    FixedPrompt { index: usize },
    FreeSpeechPreparation,
    FreeSpeech,
    ReviewRerecordPreparation { kind: RecordingTakeKind },
    ReviewRerecord { kind: RecordingTakeKind },
    Review,
}

impl Default for TrainingStep {
    fn default() -> Self {
        Self::Preparation
    }
}

impl TrainingStep {
    pub fn total_steps(prompt_count: usize) -> usize {
        prompt_count * 2 + 5
    }

    pub fn flow_total_steps(self, prompt_count: usize) -> usize {
        if self.is_review_rerecord_flow() {
            2
        } else {
            Self::total_steps(prompt_count)
        }
    }

    pub fn position(self, prompt_count: usize) -> usize {
        match self {
            Self::Preparation => 1,
            Self::AmbientSilence => 2,
            Self::FixedPromptPreparation { index } => index * 2 + 3,
            Self::FixedPrompt { index } => index * 2 + 4,
            Self::FreeSpeechPreparation => prompt_count * 2 + 3,
            Self::FreeSpeech => prompt_count * 2 + 4,
            Self::ReviewRerecordPreparation { .. } => 1,
            Self::ReviewRerecord { .. } => 2,
            Self::Review => prompt_count * 2 + 5,
        }
    }

    pub fn title(self) -> String {
        match self {
            Self::Preparation => "训练准备".to_owned(),
            Self::AmbientSilence => "环境静音".to_owned(),
            Self::FixedPromptPreparation { .. } => "固定短句准备".to_owned(),
            Self::FixedPrompt { .. } => "固定短句".to_owned(),
            Self::FreeSpeechPreparation => "自由发挥准备".to_owned(),
            Self::FreeSpeech => "自由发挥".to_owned(),
            Self::ReviewRerecordPreparation { kind } => {
                format!("重录 {}准备", kind.short_label())
            }
            Self::ReviewRerecord { kind } => format!("重录 {}", kind.short_label()),
            Self::Review => "完成确认".to_owned(),
        }
    }

    pub fn advance(self, prompt_count: usize) -> Self {
        match self {
            Self::Preparation => Self::AmbientSilence,
            Self::AmbientSilence => Self::FixedPromptPreparation { index: 0 },
            Self::FixedPromptPreparation { index } => Self::FixedPrompt { index },
            Self::FixedPrompt { index } if index + 1 < prompt_count => {
                Self::FixedPromptPreparation { index: index + 1 }
            }
            Self::FixedPrompt { .. } => Self::FreeSpeechPreparation,
            Self::FreeSpeechPreparation => Self::FreeSpeech,
            Self::FreeSpeech => Self::Review,
            Self::ReviewRerecordPreparation { kind } => Self::ReviewRerecord { kind },
            Self::ReviewRerecord { .. } => Self::Review,
            Self::Review => Self::Review,
        }
    }

    pub fn retry_previous_prompt(self, prompt_count: usize) -> Option<Self> {
        match self {
            Self::FixedPromptPreparation { index } | Self::FixedPrompt { index } if index > 0 => {
                Some(Self::FixedPromptPreparation { index: index - 1 })
            }
            Self::FreeSpeechPreparation | Self::FreeSpeech if prompt_count > 0 => {
                Some(Self::FixedPromptPreparation {
                    index: prompt_count - 1,
                })
            }
            _ => None,
        }
    }

    pub fn recording_take_kind(self) -> Option<RecordingTakeKind> {
        match self {
            Self::AmbientSilence => Some(RecordingTakeKind::AmbientSilence),
            Self::FixedPrompt { index } => Some(RecordingTakeKind::FixedPrompt { index }),
            Self::FreeSpeech => Some(RecordingTakeKind::FreeSpeech),
            Self::ReviewRerecord { kind } => Some(kind),
            _ => None,
        }
    }

    pub fn is_recording_phase(self) -> bool {
        self.recording_take_kind().is_some()
    }

    pub fn is_review_rerecord_flow(self) -> bool {
        matches!(
            self,
            Self::ReviewRerecordPreparation { .. } | Self::ReviewRerecord { .. }
        )
    }
}

#[derive(Debug)]
pub struct AppState {
    pub settings: AppSettings,
    pub config_path: String,
    pub device_inventory: DeviceInventory,
    pub last_device_error: Option<String>,
    pub last_persist_error: Option<String>,
    pub runtime_stage: RuntimeStage,
    pub runtime_metrics: RuntimeMetricsSnapshot,
    pub runtime_format: Option<RuntimeFormatSummary>,
    pub last_runtime_error: Option<String>,
    pub enrollment_script: EnrollmentScript,
    pub training_step: TrainingStep,
    pub training_step_started_at: Instant,
    pub restart_training_confirmation_clicks: u8,
    pub retry_previous_prompt_confirmation_clicks: u8,
    pub review_summary_rerecord_confirmation_target: Option<RecordingTakeKind>,
    pub review_summary_rerecord_confirmation_clicks: u8,
    pub startup_recording_prompt: Option<StartupRecordingPrompt>,
    pub startup_recording_overwrite_confirmation_clicks: u8,
    pub training_recordings: TrainingRecordingManifest,
    pub training_recording_active: bool,
    pub training_input_level_linear: f32,
    pub last_training_recording_error: Option<String>,
    pub previewing_recording_kind: Option<RecordingTakeKind>,
    pub last_training_preview_error: Option<String>,
    pub training_quality_report: Option<QualityReport>,
    pub last_training_quality_error: Option<String>,
    pub default_profile_summary: Option<ProfileSummary>,
    pub last_profile_error: Option<String>,
    pub offline_basic_filter_input_path: String,
    pub offline_basic_filter_output_path: String,
    pub last_offline_basic_filter_metrics: Option<OfflineBasicFilterMetrics>,
    pub last_offline_basic_filter_error: Option<String>,
    pub busy_state: Option<BusyState>,
    pub pending_command: Option<AppCommand>,
}

impl AppState {
    pub fn new(
        settings: AppSettings,
        config_path: String,
        device_inventory: DeviceInventory,
        last_device_error: Option<String>,
        enrollment_script: EnrollmentScript,
        default_profile_summary: Option<ProfileSummary>,
        last_profile_error: Option<String>,
        startup_recording_prompt: Option<StartupRecordingPrompt>,
    ) -> Self {
        let prompt_count = enrollment_script.prompts.len();

        Self {
            settings,
            config_path,
            device_inventory,
            last_device_error,
            last_persist_error: None,
            runtime_stage: RuntimeStage::Stopped,
            runtime_metrics: RuntimeMetricsSnapshot::default(),
            runtime_format: None,
            last_runtime_error: None,
            enrollment_script,
            training_step: TrainingStep::Preparation,
            training_step_started_at: Instant::now(),
            restart_training_confirmation_clicks: 0,
            retry_previous_prompt_confirmation_clicks: 0,
            review_summary_rerecord_confirmation_target: None,
            review_summary_rerecord_confirmation_clicks: 0,
            startup_recording_prompt,
            startup_recording_overwrite_confirmation_clicks: 0,
            training_recordings: TrainingRecordingManifest::new(prompt_count),
            training_recording_active: false,
            training_input_level_linear: 0.0,
            last_training_recording_error: None,
            previewing_recording_kind: None,
            last_training_preview_error: None,
            training_quality_report: None,
            last_training_quality_error: None,
            default_profile_summary,
            last_profile_error,
            offline_basic_filter_input_path: default_offline_basic_filter_input_path(),
            offline_basic_filter_output_path: default_offline_basic_filter_output_path(),
            last_offline_basic_filter_metrics: None,
            last_offline_basic_filter_error: None,
            busy_state: None,
            pending_command: None,
        }
    }

    pub fn queue_command(&mut self, command: AppCommand) {
        self.pending_command = Some(command);
    }

    pub fn begin_busy_action(
        &mut self,
        action: BusyAction,
        detail: impl Into<String>,
        progress: f32,
    ) {
        self.busy_state = Some(BusyState {
            action,
            detail: detail.into(),
            progress: progress.clamp(0.0, 1.0),
            started_at: Instant::now(),
        });
        self.pending_command = None;
    }

    pub fn update_busy_action(
        &mut self,
        action: BusyAction,
        detail: impl Into<String>,
        progress: f32,
    ) {
        if let Some(busy) = self
            .busy_state
            .as_mut()
            .filter(|busy| busy.action == action)
        {
            busy.detail = detail.into();
            busy.progress = progress.clamp(0.0, 1.0);
        }
    }

    pub fn finish_busy_action(&mut self) {
        self.busy_state = None;
    }

    pub fn is_busy(&self) -> bool {
        self.busy_state.is_some()
    }

    pub fn busy_state_for(&self, action: BusyAction) -> Option<&BusyState> {
        self.busy_state
            .as_ref()
            .filter(|busy| busy.action == action)
    }

    pub fn has_satisfied_busy_minimum_duration(&self, action: BusyAction) -> bool {
        self.busy_state_for(action).map_or(true, |busy| {
            busy.started_at.elapsed() >= action.minimum_visible_duration()
        })
    }

    pub fn advance_training_step(&mut self) {
        self.reset_training_confirmation_clicks();
        self.training_step = self
            .training_step
            .advance(self.enrollment_script.prompts.len());
        self.training_step_started_at = Instant::now();
    }

    pub fn restart_training_flow(&mut self) {
        self.reset_training_confirmation_clicks();
        self.training_step = TrainingStep::Preparation;
        self.training_step_started_at = Instant::now();
    }

    pub fn begin_review_rerecord(&mut self, kind: RecordingTakeKind) {
        self.reset_training_confirmation_clicks();
        self.training_step = TrainingStep::ReviewRerecordPreparation { kind };
        self.training_step_started_at = Instant::now();
    }

    pub fn load_detected_training_recordings(&mut self, manifest: TrainingRecordingManifest) {
        self.reset_training_confirmation_clicks();
        self.training_recordings = manifest;
        self.training_step = TrainingStep::Review;
        self.training_step_started_at = Instant::now();
        self.settings.selected_page = UiPage::Training;
        self.dismiss_startup_recording_prompt();
    }

    pub fn confirm_restart_training(&mut self) -> bool {
        if !matches!(self.training_step, TrainingStep::Review) {
            self.reset_restart_training_confirmation();
            return false;
        }

        self.restart_training_confirmation_clicks = self
            .restart_training_confirmation_clicks
            .saturating_add(1)
            .min(RESTART_TRAINING_CONFIRMATION_CLICKS);

        if self.restart_training_confirmation_clicks >= RESTART_TRAINING_CONFIRMATION_CLICKS {
            self.reset_restart_training_confirmation();
            true
        } else {
            false
        }
    }

    pub fn can_retry_previous_prompt(&self) -> bool {
        self.training_step
            .retry_previous_prompt(self.enrollment_script.prompts.len())
            .is_some()
    }

    pub fn confirm_retry_previous_prompt(&mut self) -> bool {
        if !self.can_retry_previous_prompt() {
            self.reset_retry_previous_prompt_confirmation();
            return false;
        }

        self.retry_previous_prompt_confirmation_clicks = self
            .retry_previous_prompt_confirmation_clicks
            .saturating_add(1)
            .min(RETRY_PREVIOUS_PROMPT_CONFIRMATION_CLICKS);

        if self.retry_previous_prompt_confirmation_clicks
            >= RETRY_PREVIOUS_PROMPT_CONFIRMATION_CLICKS
        {
            self.reset_retry_previous_prompt_confirmation();
            true
        } else {
            false
        }
    }

    pub fn retry_previous_prompt(&mut self) {
        if let Some(step) = self
            .training_step
            .retry_previous_prompt(self.enrollment_script.prompts.len())
        {
            self.reset_training_confirmation_clicks();
            self.training_step = step;
            self.training_step_started_at = Instant::now();
        }
    }

    pub fn is_training_recording_active(&self) -> bool {
        self.training_recording_active
    }

    pub fn is_review_step(&self) -> bool {
        matches!(self.training_step, TrainingStep::Review)
    }

    pub fn confirm_review_summary_rerecord(&mut self, kind: RecordingTakeKind) -> bool {
        if !self.is_review_step() {
            self.reset_review_summary_rerecord_confirmation();
            return false;
        }

        if self.review_summary_rerecord_confirmation_target != Some(kind) {
            self.review_summary_rerecord_confirmation_target = Some(kind);
            self.review_summary_rerecord_confirmation_clicks = 1;
            return false;
        }

        self.review_summary_rerecord_confirmation_clicks = self
            .review_summary_rerecord_confirmation_clicks
            .saturating_add(1)
            .min(REVIEW_SUMMARY_RERECORD_CONFIRMATION_CLICKS);

        if self.review_summary_rerecord_confirmation_clicks
            >= REVIEW_SUMMARY_RERECORD_CONFIRMATION_CLICKS
        {
            self.reset_review_summary_rerecord_confirmation();
            true
        } else {
            false
        }
    }

    pub fn review_summary_rerecord_progress(&self, kind: RecordingTakeKind) -> Option<(u8, u8)> {
        (self.review_summary_rerecord_confirmation_target == Some(kind)
            && self.review_summary_rerecord_confirmation_clicks > 0)
            .then_some((
                self.review_summary_rerecord_confirmation_clicks,
                REVIEW_SUMMARY_RERECORD_CONFIRMATION_CLICKS,
            ))
    }

    pub fn cancel_review_summary_rerecord_confirmation(&mut self) {
        self.reset_review_summary_rerecord_confirmation();
    }

    pub fn confirm_startup_recording_overwrite(&mut self) -> bool {
        if self.startup_recording_prompt.is_none() {
            self.reset_startup_recording_overwrite_confirmation();
            return false;
        }

        self.startup_recording_overwrite_confirmation_clicks = self
            .startup_recording_overwrite_confirmation_clicks
            .saturating_add(1)
            .min(STARTUP_RECORDING_OVERWRITE_CONFIRMATION_CLICKS);

        if self.startup_recording_overwrite_confirmation_clicks
            >= STARTUP_RECORDING_OVERWRITE_CONFIRMATION_CLICKS
        {
            self.reset_startup_recording_overwrite_confirmation();
            true
        } else {
            false
        }
    }

    pub fn startup_recording_overwrite_progress(&self) -> Option<(u8, u8)> {
        (self.startup_recording_prompt.is_some()
            && self.startup_recording_overwrite_confirmation_clicks > 0)
            .then_some((
                self.startup_recording_overwrite_confirmation_clicks,
                STARTUP_RECORDING_OVERWRITE_CONFIRMATION_CLICKS,
            ))
    }

    pub fn cancel_startup_recording_overwrite_confirmation(&mut self) {
        self.reset_startup_recording_overwrite_confirmation();
    }

    pub fn dismiss_startup_recording_prompt(&mut self) {
        self.startup_recording_prompt = None;
        self.reset_startup_recording_overwrite_confirmation();
    }

    pub fn restore_default_offline_basic_filter_paths(&mut self) {
        self.offline_basic_filter_input_path = default_offline_basic_filter_input_path();
        self.offline_basic_filter_output_path = default_offline_basic_filter_output_path();
    }

    fn reset_restart_training_confirmation(&mut self) {
        self.restart_training_confirmation_clicks = 0;
    }

    fn reset_retry_previous_prompt_confirmation(&mut self) {
        self.retry_previous_prompt_confirmation_clicks = 0;
    }

    fn reset_review_summary_rerecord_confirmation(&mut self) {
        self.review_summary_rerecord_confirmation_target = None;
        self.review_summary_rerecord_confirmation_clicks = 0;
    }

    fn reset_startup_recording_overwrite_confirmation(&mut self) {
        self.startup_recording_overwrite_confirmation_clicks = 0;
    }

    fn reset_training_confirmation_clicks(&mut self) {
        self.reset_restart_training_confirmation();
        self.reset_retry_previous_prompt_confirmation();
        self.reset_review_summary_rerecord_confirmation();
        self.reset_startup_recording_overwrite_confirmation();
    }
}

#[cfg(test)]
mod tests {
    use super::{AppState, TrainingStep};
    use crate::{
        audio::devices::DeviceInventory,
        config::settings::AppSettings,
        profile::record::{
            DetectedTrainingRecordings, EnrollmentScript, RecordingTakeKind,
            TrainingRecordingManifest,
        },
    };

    #[test]
    fn training_step_sequence_advances_linearly() {
        let prompt_count = 10;
        let mut step = TrainingStep::Preparation;
        let mut positions = Vec::new();

        for _ in 0..TrainingStep::total_steps(prompt_count) {
            positions.push(step.position(prompt_count));
            step = step.advance(prompt_count);
        }

        assert_eq!(positions, (1..=25).collect::<Vec<_>>());
        assert_eq!(step, TrainingStep::Review);
    }

    #[test]
    fn retry_previous_prompt_targets_previous_fixed_prompt() {
        assert_eq!(
            TrainingStep::FixedPrompt { index: 3 }.retry_previous_prompt(10),
            Some(TrainingStep::FixedPromptPreparation { index: 2 })
        );
        assert_eq!(
            TrainingStep::FixedPromptPreparation { index: 3 }.retry_previous_prompt(10),
            Some(TrainingStep::FixedPromptPreparation { index: 2 })
        );
        assert_eq!(
            TrainingStep::FreeSpeechPreparation.retry_previous_prompt(10),
            Some(TrainingStep::FixedPromptPreparation { index: 9 })
        );
        assert_eq!(
            TrainingStep::FreeSpeech.retry_previous_prompt(10),
            Some(TrainingStep::FixedPromptPreparation { index: 9 })
        );
        assert_eq!(TrainingStep::Preparation.retry_previous_prompt(10), None);
        assert_eq!(
            TrainingStep::FixedPromptPreparation { index: 0 }.retry_previous_prompt(10),
            None
        );
        assert_eq!(
            TrainingStep::FixedPrompt { index: 0 }.retry_previous_prompt(10),
            None
        );
    }

    #[test]
    fn recording_active_steps_match_guided_recording_phases() {
        let recording_steps = [
            TrainingStep::AmbientSilence,
            TrainingStep::FixedPrompt { index: 0 },
            TrainingStep::FreeSpeech,
        ];

        let non_recording_steps = [
            TrainingStep::Preparation,
            TrainingStep::FixedPromptPreparation { index: 0 },
            TrainingStep::FreeSpeechPreparation,
            TrainingStep::Review,
        ];

        for step in recording_steps {
            assert!(step.is_recording_phase());
        }

        for step in non_recording_steps {
            assert!(!step.is_recording_phase());
        }
    }

    #[test]
    fn restart_training_requires_three_review_clicks() {
        let mut state = test_app_state();
        state.training_step = TrainingStep::Review;

        assert!(!state.confirm_restart_training());
        assert_eq!(state.training_step, TrainingStep::Review);
        assert_eq!(state.restart_training_confirmation_clicks, 1);

        assert!(!state.confirm_restart_training());
        assert_eq!(state.training_step, TrainingStep::Review);
        assert_eq!(state.restart_training_confirmation_clicks, 2);

        assert!(state.confirm_restart_training());
        assert_eq!(state.training_step, TrainingStep::Review);
        assert_eq!(state.restart_training_confirmation_clicks, 0);
    }

    #[test]
    fn restart_training_confirmation_only_counts_on_review_step() {
        let mut state = test_app_state();

        assert!(!state.confirm_restart_training());
        assert_eq!(state.training_step, TrainingStep::Preparation);
        assert_eq!(state.restart_training_confirmation_clicks, 0);
    }

    #[test]
    fn retry_previous_prompt_requires_three_clicks() {
        let mut state = test_app_state();
        state.training_step = TrainingStep::FixedPrompt { index: 3 };

        assert!(!state.confirm_retry_previous_prompt());
        assert_eq!(state.training_step, TrainingStep::FixedPrompt { index: 3 });
        assert_eq!(state.retry_previous_prompt_confirmation_clicks, 1);

        assert!(!state.confirm_retry_previous_prompt());
        assert_eq!(state.training_step, TrainingStep::FixedPrompt { index: 3 });
        assert_eq!(state.retry_previous_prompt_confirmation_clicks, 2);

        assert!(state.confirm_retry_previous_prompt());
        assert_eq!(state.training_step, TrainingStep::FixedPrompt { index: 3 });
        assert_eq!(state.retry_previous_prompt_confirmation_clicks, 0);
    }

    #[test]
    fn retry_previous_prompt_confirmation_only_counts_when_retry_is_available() {
        let mut state = test_app_state();

        assert!(!state.confirm_retry_previous_prompt());
        assert_eq!(state.training_step, TrainingStep::Preparation);
        assert_eq!(state.retry_previous_prompt_confirmation_clicks, 0);
    }

    #[test]
    fn review_summary_rerecord_requires_three_clicks_on_same_clip() {
        let mut state = test_app_state();
        state.training_step = TrainingStep::Review;

        assert!(!state.confirm_review_summary_rerecord(RecordingTakeKind::AmbientSilence));
        assert_eq!(
            state.review_summary_rerecord_progress(RecordingTakeKind::AmbientSilence),
            Some((1, super::REVIEW_SUMMARY_RERECORD_CONFIRMATION_CLICKS))
        );

        assert!(!state.confirm_review_summary_rerecord(RecordingTakeKind::AmbientSilence));
        assert_eq!(
            state.review_summary_rerecord_progress(RecordingTakeKind::AmbientSilence),
            Some((2, super::REVIEW_SUMMARY_RERECORD_CONFIRMATION_CLICKS))
        );

        assert!(state.confirm_review_summary_rerecord(RecordingTakeKind::AmbientSilence));
        assert_eq!(
            state.review_summary_rerecord_progress(RecordingTakeKind::AmbientSilence),
            None
        );
    }

    #[test]
    fn review_summary_rerecord_resets_when_switching_target() {
        let mut state = test_app_state();
        state.training_step = TrainingStep::Review;

        assert!(!state.confirm_review_summary_rerecord(RecordingTakeKind::AmbientSilence));
        assert!(!state.confirm_review_summary_rerecord(RecordingTakeKind::FreeSpeech));
        assert_eq!(
            state.review_summary_rerecord_progress(RecordingTakeKind::AmbientSilence),
            None
        );
        assert_eq!(
            state.review_summary_rerecord_progress(RecordingTakeKind::FreeSpeech),
            Some((1, super::REVIEW_SUMMARY_RERECORD_CONFIRMATION_CLICKS))
        );
    }

    #[test]
    fn rerecord_flow_uses_two_step_progress_and_returns_to_review() {
        let prompt_count = 10;
        let prep = TrainingStep::ReviewRerecordPreparation {
            kind: RecordingTakeKind::FixedPrompt { index: 2 },
        };
        let record = prep.advance(prompt_count);

        assert_eq!(prep.flow_total_steps(prompt_count), 2);
        assert_eq!(prep.position(prompt_count), 1);
        assert_eq!(record.position(prompt_count), 2);
        assert_eq!(record.advance(prompt_count), TrainingStep::Review);
    }

    #[test]
    fn startup_recording_overwrite_requires_three_clicks() {
        let mut state = test_app_state();
        state.startup_recording_prompt = Some(test_startup_prompt());

        assert!(!state.confirm_startup_recording_overwrite());
        assert_eq!(state.startup_recording_overwrite_progress(), Some((1, 3)));

        assert!(!state.confirm_startup_recording_overwrite());
        assert_eq!(state.startup_recording_overwrite_progress(), Some((2, 3)));

        assert!(state.confirm_startup_recording_overwrite());
        assert_eq!(state.startup_recording_overwrite_progress(), None);
    }

    #[test]
    fn offline_basic_filter_defaults_target_default_profile_free_speech_paths() {
        let mut state = test_app_state();

        assert_eq!(
            state.offline_basic_filter_input_path,
            "profiles/default/recordings/free_speech.wav"
        );
        assert_eq!(
            state.offline_basic_filter_output_path,
            "profiles/default/offline_outputs/free_speech_basic_filter.wav"
        );

        state.offline_basic_filter_input_path = "custom-input.wav".to_owned();
        state.offline_basic_filter_output_path = "custom-output.wav".to_owned();
        state.restore_default_offline_basic_filter_paths();

        assert_eq!(
            state.offline_basic_filter_input_path,
            "profiles/default/recordings/free_speech.wav"
        );
        assert_eq!(
            state.offline_basic_filter_output_path,
            "profiles/default/offline_outputs/free_speech_basic_filter.wav"
        );
    }

    #[test]
    fn busy_action_updates_progress_and_can_finish() {
        let mut state = test_app_state();
        state.begin_busy_action(super::BusyAction::StartRealtime, "正在准备实时链路", 0.4);

        assert!(state.is_busy());
        assert!(!state.has_satisfied_busy_minimum_duration(super::BusyAction::StartRealtime));
        assert_eq!(
            state
                .busy_state_for(super::BusyAction::StartRealtime)
                .map(|busy| busy.progress),
            Some(0.4)
        );

        state.update_busy_action(super::BusyAction::StartRealtime, "正在打开音频设备", 0.75);
        assert_eq!(
            state
                .busy_state_for(super::BusyAction::StartRealtime)
                .map(|busy| (busy.detail.as_str(), busy.progress)),
            Some(("正在打开音频设备", 0.75))
        );

        state.finish_busy_action();
        assert!(!state.is_busy());
    }

    fn test_app_state() -> AppState {
        AppState::new(
            AppSettings::default(),
            String::new(),
            DeviceInventory::default(),
            None,
            EnrollmentScript {
                locale: "zh-CN",
                prompts: vec!["测试短句".to_owned(); 10],
            },
            None,
            None,
            None,
        )
    }

    fn test_startup_prompt() -> super::StartupRecordingPrompt {
        super::StartupRecordingPrompt {
            severity: super::StartupRecordingPromptSeverity::Info,
            title: "检测到之前保存的录音".to_owned(),
            summary: "测试".to_owned(),
            details: Vec::new(),
            detected_recordings: DetectedTrainingRecordings {
                manifest: TrainingRecordingManifest::new(10),
                missing_paths: Vec::new(),
                unexpected_entries: Vec::new(),
                invalid_entries: Vec::new(),
            },
        }
    }
}
