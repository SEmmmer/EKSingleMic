use std::time::Duration;

use eframe::egui::{self, FontFamily, FontId, Pos2, Rect, RichText, Shape, Stroke, Vec2};

use crate::{
    app::{
        commands::AppCommand,
        SOURCE_HAN_SANS_SC_BOLD_FAMILY,
        state::{
            AppState, RESTART_TRAINING_CONFIRMATION_CLICKS,
            RETRY_PREVIOUS_PROMPT_CONFIRMATION_CLICKS, TrainingStep,
        },
    },
    pipeline::realtime::RuntimeStage,
    profile::{
        record::{AMBIENT_SILENCE_SECONDS, FREE_SPEECH_SECONDS},
        storage::DEFAULT_PROFILE_ID,
    },
};

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    ui.heading("训练页");
    ui.label("当前使用强引导流程，只有点“确定准备好”或“确定完成”后才会进入下一步。");
    ui.separator();

    if state.is_training_recording_active() {
        ui.ctx().request_repaint_after(Duration::from_millis(33));
    }

    let total_steps = TrainingStep::total_steps(state.enrollment_script.prompts.len());
    let current_position = state.training_step.position(state.enrollment_script.prompts.len());
    let progress = current_position as f32 / total_steps as f32;
    ui.label(format!(
        "当前步骤：{current_position}/{total_steps} | {} | {} {} 句固定短句 + {} 秒自由发挥",
        state.training_step.title(),
        state.enrollment_script.locale,
        state.enrollment_script.prompts.len(),
        FREE_SPEECH_SECONDS
    ));
    ui.add(
        egui::ProgressBar::new(progress)
            .show_percentage()
            .text(format!("训练进度 {current_position}/{total_steps}")),
    );
    ui.add_space(8.0);
    show_input_device_status(ui, state);

    ui.separator();
    egui::Frame::group(ui.style()).show(ui, |ui| match state.training_step {
        TrainingStep::Preparation => show_preparation_step(ui, state),
        TrainingStep::AmbientSilence => show_ambient_step(ui, state),
        TrainingStep::FixedPromptPreparation { index } => {
            show_fixed_prompt_preparation_step(ui, state, index)
        }
        TrainingStep::FixedPrompt { index } => show_fixed_prompt_step(ui, state, index),
        TrainingStep::FreeSpeechPreparation => show_free_speech_preparation_step(ui, state),
        TrainingStep::FreeSpeech => show_free_speech_step(ui, state),
        TrainingStep::Review => show_review_step(ui, state),
    });

    ui.separator();
    show_recording_summary(ui, state);

    ui.separator();
    ui.label("默认 profile 模式：单人单机固定使用，不提供 profile 切换。");
    ui.label(format!("默认保存路径：profiles/{DEFAULT_PROFILE_ID}/speaker_profile.json"));

    if let Some(error) = &state.last_profile_error {
        ui.colored_label(
            egui::Color32::from_rgb(220, 90, 90),
            format!("默认 profile 状态读取失败: {error}"),
        );
    }

    if let Some(summary) = &state.default_profile_summary {
        ui.label("默认 profile 状态");
        ui.label(summary.label());
        ui.label(format!(
            "阈值建议: {:.3} | 创建时间: {}",
            summary.suggested_threshold, summary.created_at_utc
        ));
    } else {
        ui.label("当前还没有已保存的默认 `speaker_profile.json`。");
    }
}

fn show_preparation_step(ui: &mut egui::Ui, state: &mut AppState) {
    let has_selected_input = state.settings.selected_input_device.is_some();
    let runtime_stopped = state.runtime_stage == RuntimeStage::Stopped;

    ui.heading("第 1 步：训练准备");
    ui.label("请先确认输入设备已经选中正确的真实麦克风，再开始这轮注册。");
    ui.label("本轮流程固定为：5 秒环境静音 -> 10 句固定短句 -> 30 秒自由发挥。");
    ui.label("确认无误后再开始训练。");

    if !runtime_stopped {
        ui.colored_label(
            egui::Color32::from_rgb(220, 170, 90),
            "开始训练前请先停止设备页中的 Passthrough，避免麦克风被实时链路占用。",
        );
    }

    if ui
        .add_enabled(
            has_selected_input && runtime_stopped,
            egui::Button::new("确定准备好，开始训练"),
        )
        .clicked()
    {
        state.queue_command(AppCommand::AdvanceTrainingStep);
    }

    show_retry_previous_prompt_button(ui, state);
}

fn show_ambient_step(ui: &mut egui::Ui, state: &mut AppState) {
    ui.heading("第 2 步：环境静音");
    let remaining_seconds = ambient_countdown_remaining(state);

    if remaining_seconds == 0 {
        state.queue_command(AppCommand::AdvanceTrainingStep);
        ui.ctx().request_repaint();
        return;
    }

    ui.label("请保持环境安静，用于后续噪声基线和质量检查。");
    ui.add_space(8.0);
    ui.label(
        RichText::new(format!("{remaining_seconds}"))
            .size(42.0)
            .strong(),
    );
    ui.label("倒计时结束后，会自动进入固定短句准备。");
    ui.label("准备录制时不要说话，也尽量不要移动麦克风。");

    ui.ctx().request_repaint_after(Duration::from_millis(100));

    show_retry_previous_prompt_button(ui, state);
}

fn show_fixed_prompt_preparation_step(ui: &mut egui::Ui, state: &mut AppState, index: usize) {
    let prompt = &state.enrollment_script.prompts[index];
    let prompt_count = state.enrollment_script.prompts.len();
    let step_position = state
        .training_step
        .position(state.enrollment_script.prompts.len());

    ui.heading(format!(
        "第 {step_position} 步：固定短句准备 {:02}/{:02}",
        index + 1,
        prompt_count
    ));
    ui.label("请先看清这句文本，准备好后再开始录制本句。");
    ui.add_space(8.0);
    ui.label(RichText::new(prompt).size(28.0));
    ui.add_space(8.0);
    ui.label("确认准备好后，程序才会进入本句录制阶段。");

    if ui.button("确定准备好，开始录制本句").clicked() {
        state.queue_command(AppCommand::AdvanceTrainingStep);
    }

    show_retry_previous_prompt_button(ui, state);
}

fn show_fixed_prompt_step(ui: &mut egui::Ui, state: &mut AppState, index: usize) {
    let prompt = &state.enrollment_script.prompts[index];
    let prompt_count = state.enrollment_script.prompts.len();
    let is_last_prompt = index + 1 == prompt_count;

    let step_position = state
        .training_step
        .position(state.enrollment_script.prompts.len());
    ui.heading(format!(
        "第 {step_position} 步：固定短句 {:02}/{:02}",
        index + 1,
        prompt_count
    ));
    ui.label("请按平时直播的自然状态朗读下面这句，读完后再点确认。");
    ui.add_space(8.0);
    ui.label(RichText::new(prompt).size(28.0));
    ui.add_space(8.0);
    ui.label(format!("还剩 {} 句固定短句。", prompt_count.saturating_sub(index + 1)));

    let button_label = if is_last_prompt {
        "确认本句已完成，进入自由发挥准备"
    } else {
        "确认本句已完成，进入下一个准备阶段"
    };

    if ui.button(button_label).clicked() {
        state.queue_command(AppCommand::AdvanceTrainingStep);
    }

    show_retry_previous_prompt_button(ui, state);
}

fn show_free_speech_preparation_step(ui: &mut egui::Ui, state: &mut AppState) {
    let free_speech_step_position = state
        .training_step
        .position(state.enrollment_script.prompts.len());
    ui.heading(format!("第 {free_speech_step_position} 步：自由发挥准备"));
    ui.label("固定短句已经结束。");
    ui.label("接下来会进入 30 秒自由发挥，请先想好要说的内容，再开始。");
    ui.label("建议直接模拟平时直播中的自然表达，不需要刻意放慢或夸张发音。");

    if ui.button("确定准备好，开始 30 秒自由发挥").clicked() {
        state.queue_command(AppCommand::AdvanceTrainingStep);
    }

    show_retry_previous_prompt_button(ui, state);
}

fn show_free_speech_step(ui: &mut egui::Ui, state: &mut AppState) {
    let free_speech_step_position = state
        .training_step
        .position(state.enrollment_script.prompts.len());
    ui.heading(format!("第 {free_speech_step_position} 步：自由发挥"));
    ui.label(format!(
        "请连续自然说话 {FREE_SPEECH_SECONDS} 秒，可以做自我介绍，或者模拟平时直播时的自然口播。"
    ));
    ui.label("这一段重点覆盖连读、停顿、语速变化和自然语气。");

    if ui.button("确定已完成 30 秒自由发挥").clicked() {
        state.queue_command(AppCommand::AdvanceTrainingStep);
    }

    show_retry_previous_prompt_button(ui, state);
}

fn show_review_step(ui: &mut egui::Ui, state: &mut AppState) {
    let review_step_position = state
        .training_step
        .position(state.enrollment_script.prompts.len());
    ui.heading(format!("第 {review_step_position} 步：完成确认"));
    ui.label("这轮强引导训练步骤已经全部走完。");
    ui.label("当前录音文件已经落地。下一步会在这里接入质量检查、embedding 聚合和默认 profile 保存。");
    ui.label(format!(
        "如需丢弃本轮训练信息并重新开始，需要连续点击 {RESTART_TRAINING_CONFIRMATION_CLICKS} 次。"
    ));

    if ui.button("重新开始本轮训练").clicked() {
        if state.confirm_restart_training() {
            state.queue_command(AppCommand::RestartTrainingFlow);
        }
    }

    ui.small(format!(
        "当前确认次数：{}/{}",
        state.restart_training_confirmation_clicks, RESTART_TRAINING_CONFIRMATION_CLICKS
    ));
}

fn ambient_countdown_remaining(state: &AppState) -> u32 {
    let elapsed = state.training_step_started_at.elapsed().as_secs_f32();
    let remaining = (AMBIENT_SILENCE_SECONDS as f32 - elapsed).max(0.0);
    remaining.ceil() as u32
}

fn show_input_device_status(ui: &mut egui::Ui, state: &AppState) {
    if let Some(input) = state.settings.selected_input_device.as_deref() {
        ui.label(format!("当前输入设备：{input}"));
    } else {
        ui.colored_label(
            egui::Color32::from_rgb(220, 90, 90),
            "当前输入设备：未选择，请先到设备页选择真实麦克风。",
        );
    }

    let (status_text, status_color) = if state.is_training_recording_active() {
        ("当前麦克风录制中", egui::Color32::from_rgb(70, 170, 90))
    } else {
        ("当前麦克风未录制", egui::Color32::from_rgb(220, 90, 90))
    };

    let status_font = FontId::new(18.0, FontFamily::Name(SOURCE_HAN_SANS_SC_BOLD_FAMILY.into()));
    let status_galley = ui.fonts_mut(|fonts| {
        fonts.layout_no_wrap(status_text.to_owned(), status_font.clone(), status_color)
    });
    let row_height = status_galley.size().y.max(18.0);

    ui.horizontal(|ui| {
        draw_status_icon(ui, state.is_training_recording_active(), status_color, row_height);
        ui.label(
            RichText::new(status_text)
                .font(status_font)
                .color(status_color),
        );
        ui.add_space(10.0);
        draw_level_meter(
            ui,
            state.training_input_level_linear,
            state.is_training_recording_active(),
            row_height,
        );
    });

    if let Some(error) = &state.last_training_recording_error {
        ui.colored_label(
            egui::Color32::from_rgb(220, 90, 90),
            format!("训练录音错误：{error}"),
        );
    }
}

fn draw_status_icon(ui: &mut egui::Ui, is_recording: bool, color: egui::Color32, row_height: f32) {
    let icon_slot_size = Vec2::new(18.0, row_height);
    let (slot_rect, _) = ui.allocate_exact_size(icon_slot_size, egui::Sense::hover());
    let rect = Rect::from_center_size(slot_rect.center(), Vec2::new(18.0, 18.0));
    let painter = ui.painter();

    if is_recording {
        let triangle = vec![
            Pos2::new(rect.left() + 4.0, rect.top() + 3.0),
            Pos2::new(rect.left() + 4.0, rect.bottom() - 3.0),
            Pos2::new(rect.right() - 3.0, rect.center().y),
        ];
        painter.add(Shape::convex_polygon(triangle, color, Stroke::NONE));
    } else {
        let left_bar = Rect::from_min_max(
            Pos2::new(rect.left() + 4.0, rect.top() + 3.0),
            Pos2::new(rect.left() + 7.0, rect.bottom() - 3.0),
        );
        let right_bar = Rect::from_min_max(
            Pos2::new(rect.right() - 7.0, rect.top() + 3.0),
            Pos2::new(rect.right() - 4.0, rect.bottom() - 3.0),
        );
        painter.rect_filled(left_bar, 1.0, color);
        painter.rect_filled(right_bar, 1.0, color);
    }
}

fn draw_level_meter(ui: &mut egui::Ui, level_linear: f32, is_recording: bool, row_height: f32) {
    let meter_slot_size = Vec2::new(156.0, row_height);
    let (slot_rect, _) = ui.allocate_exact_size(meter_slot_size, egui::Sense::hover());
    let meter_rect = Rect::from_center_size(slot_rect.center(), Vec2::new(156.0, 12.0));
    let normalized_level = level_linear.clamp(0.0, 1.0);
    let fill_width = meter_rect.width() * normalized_level;

    let background_color = egui::Color32::from_rgb(42, 46, 52);
    let border_color = egui::Color32::from_rgb(84, 90, 98);
    let fill_color = if !is_recording {
        egui::Color32::from_rgb(88, 92, 98)
    } else if normalized_level < 0.55 {
        egui::Color32::from_rgb(70, 170, 90)
    } else if normalized_level < 0.82 {
        egui::Color32::from_rgb(220, 170, 90)
    } else {
        egui::Color32::from_rgb(220, 90, 90)
    };

    ui.painter().rect(
        meter_rect,
        3.0,
        background_color,
        Stroke::new(1.0, border_color),
        egui::StrokeKind::Outside,
    );

    if fill_width > 0.0 {
        let fill_rect = Rect::from_min_size(meter_rect.min, Vec2::new(fill_width, meter_rect.height()));
        ui.painter().rect_filled(fill_rect, 3.0, fill_color);
    }
}

fn show_recording_summary(ui: &mut egui::Ui, state: &AppState) {
    ui.label("当前录音结果");

    if let Some(ambient) = &state.training_recordings.ambient_silence {
        ui.label(format!(
            "环境静音：已录制 {:.1} 秒 | {}",
            ambient.duration_seconds, ambient.relative_path
        ));
    } else {
        ui.label("环境静音：未录制");
    }

    egui::CollapsingHeader::new(format!(
        "固定短句：已录制 {}/{} 句",
        state.training_recordings.recorded_prompt_count(),
        state.training_recordings.fixed_prompts.len()
    ))
    .default_open(false)
    .show(ui, |ui| {
        for (index, prompt) in state.enrollment_script.prompts.iter().enumerate() {
            let clip = state
                .training_recordings
                .fixed_prompts
                .get(index)
                .and_then(|entry| entry.as_ref());
            let status_color = if clip.is_some() {
                egui::Color32::from_rgb(70, 170, 90)
            } else {
                egui::Color32::from_rgb(150, 150, 150)
            };
            let status_text = if let Some(clip) = clip {
                format!("已录制 {:.1} 秒 | {}", clip.duration_seconds, clip.relative_path)
            } else {
                "未录制".to_owned()
            };

            ui.label(RichText::new(format!("{:02}. {}", index + 1, prompt)).strong());
            ui.colored_label(status_color, status_text);
            ui.add_space(6.0);
        }
    });

    if let Some(free_speech) = &state.training_recordings.free_speech {
        ui.label(format!(
            "自由发挥：已录制 {:.1} 秒 | {}",
            free_speech.duration_seconds, free_speech.relative_path
        ));
    } else {
        ui.label("自由发挥：未录制");
    }
}

fn show_retry_previous_prompt_button(ui: &mut egui::Ui, state: &mut AppState) {
    ui.add_space(12.0);
    let can_retry = state.can_retry_previous_prompt();

    if ui
        .add_enabled(
            can_retry,
            egui::Button::new("有一段没录好，重新录制上一句话"),
        )
        .clicked()
    {
        if state.confirm_retry_previous_prompt() {
            state.queue_command(AppCommand::RetryPreviousPrompt);
        }
    }

    if !can_retry {
        ui.small("当前还没有上一句话可重录。");
    } else {
        ui.small(format!(
            "需要连续点击 {} 次才会回退并重新录制上一句话。",
            RETRY_PREVIOUS_PROMPT_CONFIRMATION_CLICKS
        ));
        ui.small(format!(
            "当前确认次数：{}/{}",
            state.retry_previous_prompt_confirmation_clicks,
            RETRY_PREVIOUS_PROMPT_CONFIRMATION_CLICKS
        ));
    }
}
