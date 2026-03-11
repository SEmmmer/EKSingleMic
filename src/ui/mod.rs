pub mod debug;
pub mod devices;
pub mod inference;
pub mod training;

use eframe::egui;

use crate::{app::state::AppState, config::settings::UiPage};

pub fn show_navigation(ui: &mut egui::Ui, state: &mut AppState) {
    ui.heading("导航");
    ui.label("M0 先交付可运行 GUI 壳子。");
    ui.separator();

    for page in UiPage::ALL {
        if ui
            .selectable_label(state.settings.selected_page == page, page.label())
            .clicked()
        {
            state.settings.selected_page = page;
        }
    }
}

pub fn show_page(ui: &mut egui::Ui, state: &mut AppState) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| match state.settings.selected_page {
            UiPage::Devices => devices::show(ui, state),
            UiPage::Training => training::show(ui, state),
            UiPage::Inference => inference::show(ui, state),
            UiPage::Debug => debug::show(ui, state),
        });
}
