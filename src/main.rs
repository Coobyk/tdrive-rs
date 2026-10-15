mod db;
mod format;
mod models;
mod tele;
mod theme;
mod ui;

use gpui::{App, AppContext};
use gpui_component::Root;
use gpui_platform::application;
use ui::TDriveView;

fn main() {
    application()
        .with_assets(gpui_kit_assets::AllAssets)
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            gpui_component::Theme::change(gpui_component::ThemeMode::Dark, None, cx);
            let bounds =
                gpui::Bounds::centered(None, gpui::size(gpui::px(1360.0), gpui::px(860.0)), cx);
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("TDrive".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    let view = cx.new(|cx| TDriveView::new(window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )
            .unwrap();
            cx.activate(true);
        });
}
