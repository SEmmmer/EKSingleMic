#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppCommand {
    RefreshAudioDevices,
    UseDetectedVirtualCableOutput,
    StartRealtime,
    StopRealtime,
    AdvanceTrainingStep,
    RetryPreviousPrompt,
    RestartTrainingFlow,
}
