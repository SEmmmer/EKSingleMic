use crate::profile::record::RecordingTakeKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppCommand {
    RefreshAudioDevices,
    UseDetectedVirtualCableOutput,
    StartRealtime,
    StopRealtime,
    RunOfflineBasicFilter,
    AdvanceTrainingStep,
    RetryPreviousPrompt,
    RestartTrainingFlow,
    StartReviewRerecord { kind: RecordingTakeKind },
    PreviewRecordedClip { kind: RecordingTakeKind },
    LoadDetectedTrainingRecordings,
    OverwriteDetectedTrainingRecordings,
}
