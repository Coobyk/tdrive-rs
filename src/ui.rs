//! The TDrive desktop shell.
//!
//! This mirrors the Svelte dashboard in `frontend/src/ui/AppShell.svelte` and
//! its stylesheets: a 240px drive sidebar, a header with the search field and
//! the upload action, a breadcrumb, and the sortable Name / Date / Size /
//! Actions file table. Colours come from [`crate::theme`], which carries the
//! original palette, so nothing renders light-on-light.

use std::path::PathBuf;

use chrono::Utc;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, FontWeight, IntoElement, ParentElement, Render, Rgba,
    SharedString, Styled, Subscription, Window, div, px,
};
use gpui_component::{
    Icon,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
};
use gpui_kit_assets::IconName;

use crate::format::{entry_origin, human_size, purge_countdown, short_date};
use crate::models::{
    AppState, DialogKind, FileKind, LocalState, LoginStep, RemoteFile, SortKey, SortState, ViewMode,
};
use crate::tele;
use crate::theme;

type TeleClient = grammers_client::Client;

pub struct TDriveView {
    state: AppState,
    api_id_input: Entity<InputState>,
    api_hash_input: Entity<InputState>,
    phone_input: Entity<InputState>,
    code_input: Entity<InputState>,
    password_input: Entity<InputState>,
    search_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl TDriveView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let api_id_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("API ID (from my.telegram.org)"));
        let api_hash_input = cx.new(|cx| InputState::new(window, cx).placeholder("API hash"));
        let phone_input = cx.new(|cx| InputState::new(window, cx).placeholder("+1 555 0100"));
        let code_input = cx.new(|cx| InputState::new(window, cx).placeholder("Login code"));
        let password_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Two-factor password"));
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search files..."));

        let mut this = Self {
            state: AppState::default(),
            api_id_input,
            api_hash_input,
            phone_input,
            code_input,
            password_input,
            search_input,
            _subscriptions: Vec::new(),
        };

        // A development affordance: fills the shell with representative data so
        // the dashboard can be built, run and screenshotted without a live
        // Telegram session.
        if std::env::var("TDRIVE_DEMO").is_ok() {
            this.state.seed_demo();
        }

        let subscription = cx.subscribe(
            &this.search_input,
            |this: &mut Self, _emitter, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.state.query = this.search_input.read(cx).value().to_string();
                    cx.notify();
                }
            },
        );
        this._subscriptions.push(subscription);
        this
    }
}

// --- Small building blocks --------------------------------------------------

/// A bundled icon at an explicit size and colour.
fn glyph(name: IconName, size: f32, color: Rgba) -> Icon {
    Icon::new(name).size(px(size)).text_color(color)
}

/// The product mark: the paper plane the original draws over a cloud, kept as a
/// vector so the window needs no raster asset.
fn logo_mark() -> impl IntoElement {
    div()
        .w(px(34.))
        .h(px(28.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .bg(theme::accent())
        .child(glyph(IconName::Send, 17., theme::text()))
}

fn section_title(text: &'static str) -> impl IntoElement {
    div()
        .px(px(12.))
        .pb(px(6.))
        .text_size(px(theme::TYPE_XS))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text_muted())
        .child(text)
}

fn drive_empty(text: &'static str) -> impl IntoElement {
    div()
        .px(px(12.))
        .py(px(6.))
        .text_size(px(12.8))
        .text_color(theme::text_muted())
        .opacity(0.6)
        .child(text)
}

/// One row in a sidebar list: an 18px glyph, then the title.
fn sidebar_item(
    id: SharedString,
    icon: IconName,
    label: String,
    active: bool,
    on_click: impl Fn(&mut App) + 'static,
) -> AnyElement {
    let color = if active {
        theme::accent()
    } else {
        theme::text_muted()
    };
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(label.clone())
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.))
        .px(px(12.))
        .py(px(9.))
        .rounded(px(8.))
        .cursor_pointer()
        .text_size(px(theme::TYPE_BASE))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .when(active, |el| el.bg(theme::overlay_accent_1()))
        .hover(|style| style.bg(theme::panel()))
        .on_click(move |_, _window, cx| on_click(cx))
        .child(glyph(icon, 18., color))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(if active {
                    theme::accent()
                } else {
                    theme::text_soft()
                })
                .child(label),
        )
        .into_any_element()
}

fn drive_action_button(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    on_click: impl Fn(&mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(label)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.))
        .px(px(12.))
        .py(px(8.))
        .rounded(px(8.))
        .border_1()
        .border_color(theme::border_soft())
        .cursor_pointer()
        .text_size(px(theme::TYPE_SM))
        .text_color(theme::text_muted())
        .hover(|style| style.bg(theme::panel()).text_color(theme::text_soft()))
        .on_click(move |_, _window, cx| on_click(cx))
        .child(glyph(icon, 16., theme::text_muted()))
        .child(label)
        .into_any_element()
}

/// The 32px square that leads a file row. Folders keep the accent -- they are
/// the rows you can go into -- while files stay neutral and let the glyph carry
/// the type, exactly as `files.css` explains.
fn file_chip(kind: FileKind) -> impl IntoElement {
    let folder = kind == FileKind::Folder;
    div()
        .w(px(theme::TILE))
        .h(px(theme::TILE))
        .mr(px(theme::COL_GAP))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .bg(if folder {
            theme::overlay_accent_1()
        } else {
            theme::overlay_white_1()
        })
        .child(glyph(
            kind_icon(kind),
            18.,
            if folder {
                theme::accent()
            } else {
                theme::text()
            },
        ))
}

fn kind_icon(kind: FileKind) -> IconName {
    match kind {
        FileKind::Folder => IconName::Folder,
        FileKind::Document => IconName::TextAlignStart,
        FileKind::Image => IconName::Image,
        FileKind::Video => IconName::Video,
        FileKind::Audio => IconName::Music,
        FileKind::Sheet => IconName::Table,
        FileKind::Code => IconName::Code,
        FileKind::Archive => IconName::Archive,
        FileKind::Other => IconName::File,
    }
}

/// A quiet 28px icon button: no tile of its own, a soft background on hover,
/// and `danger` for the delete affordance.
///
/// `group` names the row this action belongs to; `hidden` marks the actions the
/// original keeps invisible until the row is hovered, focused or selected.
fn row_action(
    id: SharedString,
    group: SharedString,
    hidden: bool,
    icon: IconName,
    danger: bool,
    tooltip: &'static str,
    on_click: impl Fn(&mut App) + 'static,
) -> AnyElement {
    let base = if danger {
        theme::danger()
    } else {
        theme::text_muted()
    };
    let button = div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(tooltip)
        .w(px(theme::ACTION))
        .h(px(theme::ACTION))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .cursor_pointer()
        .text_color(base)
        .hover(move |style| {
            if danger {
                style.bg(theme::overlay_danger_1())
            } else {
                style.bg(theme::overlay_2()).text_color(theme::text())
            }
        })
        // Stop the row beneath from also handling the click: the row's own
        // handler clears any pending delete confirmation.
        .on_click(move |_, _window, cx| {
            cx.stop_propagation();
            on_click(cx);
        })
        .child(glyph(
            icon,
            16.,
            if danger {
                theme::danger()
            } else {
                theme::text_muted()
            },
        ));

    if hidden {
        button
            .opacity(0.)
            .group_hover(group, |style| style.opacity(1.))
            .into_any_element()
    } else {
        button.into_any_element()
    }
}

// --- Render -----------------------------------------------------------------

impl Render for TDriveView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = if self.state.login.step == LoginStep::LoggedIn {
            self.render_app(cx).into_any_element()
        } else {
            self.render_login(cx).into_any_element()
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::canvas())
            .text_color(theme::text_soft())
            .child(content)
    }
}

impl TDriveView {
    fn brand(&self) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(9.))
            .child(logo_mark())
            .child(
                div()
                    .text_size(px(19.))
                    .font_weight(FontWeight::EXTRA_BOLD)
                    .text_color(theme::text())
                    .child("TDrive"),
            )
    }

    // --- Login --------------------------------------------------------------

    fn render_login(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let step = self.state.login.step;
        let view = cx.entity();
        let error = self.state.login.error.clone();
        let working = self.state.login.working.clone();

        let (title, subtitle, fields): (&str, &str, Vec<AnyElement>) = match step {
            LoginStep::NeedCredentials => (
                "Sign in to Telegram",
                "Enter the API credentials for your Telegram application.",
                vec![
                    field("API ID", text_field(&self.api_id_input)),
                    field("API HASH", text_field(&self.api_hash_input)),
                ],
            ),
            LoginStep::NeedPhone => (
                "Confirm your number",
                "Telegram will send a login code to this number.",
                vec![field("PHONE NUMBER", text_field(&self.phone_input))],
            ),
            LoginStep::NeedCode => (
                "Enter the code",
                "The code Telegram just sent, without spaces.",
                vec![field("LOGIN CODE", text_field(&self.code_input))],
            ),
            LoginStep::NeedPassword => (
                "Two-step verification",
                "Your account is protected by a password.",
                vec![field("PASSWORD", text_field(&self.password_input))],
            ),
            LoginStep::LoggedIn => ("", "", Vec::new()),
        };

        let action = match step {
            LoginStep::NeedCredentials => {
                login_button("connect", "Connect", view.clone(), LoginAction::Connect)
            }
            LoginStep::NeedPhone => login_button(
                "send-code",
                "Send code",
                view.clone(),
                LoginAction::SendCode,
            ),
            LoginStep::NeedCode => {
                login_button("verify", "Verify", view.clone(), LoginAction::Verify)
            }
            LoginStep::NeedPassword => {
                login_button("unlock", "Unlock", view.clone(), LoginAction::Unlock)
            }
            LoginStep::LoggedIn => Button::new("noop").ghost(),
        };

        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(420.))
                    .flex()
                    .flex_col()
                    .gap(px(20.))
                    .p(px(32.))
                    .rounded(px(12.))
                    .bg(theme::surface_1())
                    .border_1()
                    .border_color(theme::border_soft())
                    .child(self.brand())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(theme::TYPE_MD))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text())
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::TYPE_SM))
                                    .text_color(theme::text_muted())
                                    .child(subtitle),
                            ),
                    )
                    .children(fields)
                    .children(error.map(|message| {
                        div()
                            .rounded(px(8.))
                            .bg(theme::overlay_danger_1())
                            .border_1()
                            .border_color(theme::overlay_danger_2())
                            .px(px(12.))
                            .py(px(8.))
                            .text_size(px(theme::TYPE_SM))
                            .text_color(theme::danger())
                            .child(message)
                    }))
                    .child(action)
                    .children(working.map(|message| {
                        div()
                            .text_size(px(theme::TYPE_SM))
                            .text_color(theme::text_muted())
                            .child(message)
                    })),
            )
    }

    // --- Application shell --------------------------------------------------

    fn render_app(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_row()
            .child(self.render_sidebar(cx))
            .child(self.render_main(cx))
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        let (personal, shared, active) = {
            let state = &self.state;
            (
                drive_rows(state, DialogKind::Personal),
                drive_rows(state, DialogKind::Shared),
                state.active_drive,
            )
        };
        let view_mode = self.state.view;
        let loading = self.state.dialogs_loading;
        let status = self.state.status.clone();
        let used = self.state.total_size();

        let personal_items: Vec<AnyElement> = personal
            .into_iter()
            .map(|(index, title)| {
                let row_view = view.clone();
                sidebar_item(
                    SharedString::from(format!("drive-{index}")),
                    IconName::Folder,
                    title,
                    active == Some(index),
                    move |cx| open_drive_clicked(row_view.clone(), index, cx),
                )
            })
            .collect();

        let shared_items: Vec<AnyElement> = shared
            .into_iter()
            .map(|(index, title)| {
                let row_view = view.clone();
                sidebar_item(
                    SharedString::from(format!("drive-{index}")),
                    IconName::Folder,
                    title,
                    active == Some(index),
                    move |cx| open_drive_clicked(row_view.clone(), index, cx),
                )
            })
            .collect();

        let photos_view = view.clone();
        let trash_view = view.clone();
        let new_drive_view = view.clone();
        let join_view = view.clone();

        let my_drive = div()
            .flex()
            .flex_col()
            .child(section_title("My Drive"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(personal_items)
                    .child(sidebar_item(
                        SharedString::from("nav-photos"),
                        IconName::Images,
                        "Photos".to_string(),
                        view_mode == ViewMode::Photos,
                        move |cx| {
                            photos_view.update(cx, |this, cx| {
                                this.state.view = ViewMode::Photos;
                                this.state.pending_delete = None;
                                this.state.pending_purge = None;
                                this.state.confirm_empty_trash = false;
                                cx.notify();
                            });
                        },
                    ))
                    .child(sidebar_item(
                        SharedString::from("nav-trash"),
                        IconName::Trash,
                        "Trash".to_string(),
                        view_mode == ViewMode::Trash,
                        move |cx| {
                            trash_view.update(cx, |this, cx| {
                                this.state.view = ViewMode::Trash;
                                this.state.pending_delete = None;
                                this.state.pending_purge = None;
                                this.state.confirm_empty_trash = false;
                                this.state.selected_file = None;
                                this.state.status = if this.state.trash.is_empty() {
                                    "Trash is empty".to_string()
                                } else {
                                    format!("{} items in trash", this.state.trash.len())
                                };
                                cx.notify();
                            });
                        },
                    )),
            );

        let shared_section = div()
            .flex()
            .flex_col()
            .child(section_title("Shared with me"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(shared_items)
                    .children(loading.then(|| drive_empty("Loading...")))
                    .children(
                        (!loading && self.state.dialogs.is_empty())
                            .then(|| drive_empty("No shared drives yet")),
                    ),
            );

        div()
            .w(px(theme::SIDEBAR_W))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .p(px(theme::SIDEBAR_PAD))
            .bg(theme::surface_0())
            .border_r_1()
            .border_color(theme::border_soft())
            .child(div().mb(px(theme::LOGO_MB)).child(self.brand()))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("drives-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .pb(px(18.))
                            .flex()
                            .flex_col()
                            .gap(px(18.))
                            .child(my_drive)
                            .child(shared_section),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .pt(px(14.))
                            .pb(px(18.))
                            .child(drive_action_button(
                                "open-new-drive",
                                IconName::FolderPlus,
                                "New shared drive",
                                move |cx| {
                                    new_drive_view.update(cx, |this, cx| {
                                        this.state.status =
                                            "Creating a shared drive is not wired up yet"
                                                .to_string();
                                        cx.notify();
                                    });
                                },
                            ))
                            .child(drive_action_button(
                                "open-join-drive",
                                IconName::Link2,
                                "Join with link",
                                move |cx| {
                                    join_view.update(cx, |this, cx| {
                                        this.state.status =
                                            "Joining with an invite link is not wired up yet"
                                                .to_string();
                                        cx.notify();
                                    });
                                },
                            )),
                    ),
            )
            .child(
                div()
                    .pt(px(20.))
                    .border_t_1()
                    .border_color(theme::border_soft())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_between()
                            .gap_2()
                            .text_size(px(theme::TYPE_XS))
                            .text_color(theme::text_muted())
                            .child("Storage")
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .child(format!("{} / Unlimited", human_size(used))),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(theme::TYPE_XS))
                            .text_color(theme::text_subtle())
                            .truncate()
                            .child(status),
                    ),
            )
            .into_any_element()
    }

    fn render_main(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        let trash_mode = self.state.trash_mode();
        let photos_mode = self.state.photos_only();
        let (title, loading, can_upload, error) = {
            let state = &self.state;
            (
                state
                    .active_dialog()
                    .map(|d| d.title.clone())
                    .unwrap_or_else(|| "My Drive".to_string()),
                state.files_loading,
                state.active_drive.is_some() && !trash_mode,
                state.error.clone(),
            )
        };
        let trash_count = self.state.trash.len();
        let empty_armed = self.state.confirm_empty_trash;

        let upload_view = view.clone();
        let reload_view = view.clone();

        let header = div()
            .h(px(theme::HEADER_H))
            .flex_shrink_0()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(16.))
            .px(px(24.))
            .border_b_1()
            .border_color(theme::border_soft())
            .child(
                div()
                    .w(px(theme::SEARCH_W))
                    .min_w_0()
                    .flex_shrink_0()
                    .child(
                        Input::new(&self.search_input)
                            .prefix(glyph(IconName::Search, 16., theme::text_muted()))
                            .cleanable(true),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.))
                    .flex_shrink_0()
                    // Nothing can be added to the trash, so the controls that
                    // add or reload a live drive go, as in the original shell.
                    .children((!trash_mode).then(|| {
                        Button::new("reload")
                            .ghost()
                            .icon(IconName::RefreshCw)
                            .tooltip("Reload this drive")
                            .on_click(move |_, _window, cx| {
                                if let Some(index) = reload_view.read(cx).state.active_drive {
                                    open_drive_clicked(reload_view.clone(), index, cx);
                                }
                            })
                    }))
                    .children((!trash_mode).then(|| {
                        let button = Button::new("upload")
                            .primary()
                            .icon(IconName::Upload)
                            .label("Upload");
                        if can_upload {
                            button.on_click(move |_, _window, cx| {
                                spawn_upload(upload_view.clone(), cx);
                            })
                        } else {
                            button
                        }
                    })),
            );

        let back = div()
            .w(px(32.))
            .h(px(32.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .opacity(0.35)
            .child(glyph(IconName::ChevronLeft, 18., theme::text_soft()));

        let trail = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .min_w_0()
            .child(
                div()
                    .text_size(px(theme::TYPE_BASE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child("My Drive"),
            )
            .child(div().text_color(theme::text_muted()).child("/"))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::TYPE_BASE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child(title.clone()),
            );

        // The trash shows a title where the drive shows a trail, at the same
        // weight and scale as the Photos destination it sits beside.
        let trash_heading = div()
            .flex_shrink_0()
            .text_size(px(theme::TYPE_LG))
            .font_weight(FontWeight::BOLD)
            .text_color(theme::text())
            .child("Trash");

        let breadcrumb = div()
            .h(px(theme::BREADCRUMB_H))
            .flex_shrink_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .px(px(24.))
            .border_b_1()
            .border_color(theme::border_soft())
            .bg(theme::breadcrumb_bg())
            // There is no folder to be at in the trash, so there is no trail to
            // show and nowhere for Back to go: leaving is the sidebar's job.
            .children((!trash_mode).then_some(back))
            .children((!trash_mode).then_some(trail))
            .children(trash_mode.then_some(trash_heading))
            .child(div().flex_1())
            // Destructive and irreversible, so it only appears when there is
            // something to empty and only arms on the first click.
            .children(
                (trash_mode && trash_count > 0)
                    .then(|| empty_trash_button(view.clone(), empty_armed)),
            )
            .children((photos_mode && !trash_mode).then(|| {
                div()
                    .flex_shrink_0()
                    .px(px(10.))
                    .py(px(2.))
                    .rounded(px(999.))
                    .bg(theme::overlay_accent_1())
                    .text_size(px(theme::TYPE_XS))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::accent())
                    .child("Photos")
            }));
        let sort = self.state.sort;
        let date_label = if trash_mode { "Time left" } else { "Date" };
        let table_header = div()
            .h(px(theme::TABLE_HEADER_H))
            .flex_shrink_0()
            .flex()
            .flex_row()
            .items_center()
            .px(px(30.))
            .gap(px(theme::COL_GAP))
            .text_size(px(theme::TYPE_XS))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::text_muted())
            .child(
                div()
                    .flex_1()
                    .flex_grow(2.)
                    .min_w_0()
                    .pl(px(theme::NAME_INSET))
                    .child(sort_button(view.clone(), SortKey::Name, "Name", sort)),
            )
            .child(div().flex_1().min_w(px(96.)).child(sort_button(
                view.clone(),
                SortKey::Date,
                date_label,
                sort,
            )))
            .child(div().flex_1().min_w(px(84.)).child(sort_button(
                view.clone(),
                SortKey::Size,
                "Size",
                sort,
            )))
            .child(
                div()
                    .w(px(theme::COL_ACTIONS_W))
                    .flex_shrink_0()
                    .child("Actions"),
            );

        let banner = error.map(|message| {
            div()
                .flex_shrink_0()
                .mx(px(12.))
                .mt(px(8.))
                .rounded(px(8.))
                .bg(theme::overlay_danger_1())
                .border_1()
                .border_color(theme::overlay_danger_2())
                .px(px(12.))
                .py(px(8.))
                .text_size(px(theme::TYPE_SM))
                .text_color(theme::danger())
                .child(message)
        });

        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(theme::canvas())
            .child(header)
            .child(breadcrumb)
            .children(banner)
            .child(table_header)
            .child(self.render_file_list(loading, cx))
            .into_any_element()
    }

    fn render_file_list(&self, loading: bool, cx: &mut Context<Self>) -> AnyElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        if self.state.trash_mode() {
            for index in self.state.visible_trash() {
                rows.push(self.render_trash_row(index, cx));
            }
        } else if loading {
            rows.extend((0..7).map(skeleton_row));
        } else {
            for index in self.state.visible_files() {
                rows.push(self.render_file_row(index, cx));
            }
        }
        let empty = !loading && rows.is_empty();

        div()
            .id("file-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(10.))
            .flex()
            .flex_col()
            .children(rows)
            .children(empty.then(|| self.empty_state()))
            .into_any_element()
    }

    fn empty_state(&self) -> AnyElement {
        let (icon, title, body) = if self.state.trash_mode() {
            (
                IconName::Trash,
                "Nothing in the trash",
                "Anything you delete waits here until its time runs out, so you can put it back.",
            )
        } else if self.state.active_drive.is_none() {
            (
                IconName::HardDrive,
                "No drive selected",
                "Pick a drive on the left to see its files.",
            )
        } else if self.state.photos_only() {
            (
                IconName::Images,
                "No photos here",
                "This drive has no images.",
            )
        } else if !self.state.query.trim().is_empty() {
            (
                IconName::SearchX,
                "No matches",
                "No file in this drive matches the search.",
            )
        } else {
            (
                IconName::Inbox,
                "This drive is empty",
                "Upload a file to put it in this drive.",
            )
        };
        div()
            .min_h(px(220.))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .px(px(20.))
            .py(px(46.))
            .child(
                div()
                    .w(px(40.))
                    .h(px(40.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(12.))
                    .bg(theme::overlay_white_1())
                    .border_1()
                    .border_color(theme::border_soft())
                    .child(glyph(icon, 22., theme::text_subtle())),
            )
            .child(
                div()
                    .text_size(px(theme::TYPE_MD))
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme::text())
                    .child(title),
            )
            .child(
                div()
                    .max_w(px(420.))
                    .text_size(px(theme::TYPE_SM))
                    .text_color(theme::text_muted())
                    .child(body),
            )
            .into_any_element()
    }

    fn render_file_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        let file = self.state.files[index].clone();
        let selected = self.state.selected_file == Some(index);
        let confirming = self.state.pending_delete == Some(file.message_id);
        // The name a row hands its actions so they can reveal themselves on
        // hover; unique per row, because group names are a global registry.
        let group = SharedString::from(format!("row-{index}"));
        // A quiet row keeps its actions invisible until it is hovered, focused
        // or selected, exactly as `files.css` describes.
        let hidden = !(selected || confirming);

        let open_view = view.clone();
        let download_view = view.clone();
        let ask_delete_view = view.clone();
        let confirm_view = view.clone();
        let cancel_view = view.clone();

        let date = short_date(file.date);
        let size = if file.kind == FileKind::Folder {
            "—".to_string()
        } else {
            human_size(file.size)
        };
        let meta = match &file.local_state {
            LocalState::Remote => None,
            LocalState::Downloading { .. } => Some(("Downloading…", theme::text_muted())),
            LocalState::Downloaded(_) => Some(("Saved", theme::success())),
            LocalState::Failed(_) => Some(("Failed", theme::danger())),
        };

        let actions = div()
            .w(px(theme::COL_ACTIONS_W))
            .flex_shrink_0()
            .flex()
            .flex_row()
            .items_center()
            .justify_end()
            .gap(px(4.))
            .children(meta.map(|(text, color)| {
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::TYPE_XS))
                    .text_color(color)
                    .child(text)
            }))
            .child(if confirming {
                row_action(
                    SharedString::from(format!("confirm-{index}")),
                    group.clone(),
                    false,
                    IconName::Check,
                    false,
                    "Confirm delete",
                    move |cx| spawn_delete(confirm_view.clone(), index, cx),
                )
            } else {
                row_action(
                    SharedString::from(format!("download-{index}")),
                    group.clone(),
                    hidden,
                    IconName::Download,
                    false,
                    "Download",
                    move |cx| spawn_download(download_view.clone(), index, cx),
                )
            })
            .child(if confirming {
                row_action(
                    SharedString::from(format!("cancel-{index}")),
                    group.clone(),
                    false,
                    IconName::X,
                    false,
                    "Cancel",
                    move |cx| {
                        cancel_view.update(cx, |this, cx| {
                            this.state.pending_delete = None;
                            cx.notify();
                        });
                    },
                )
            } else {
                row_action(
                    SharedString::from(format!("delete-{index}")),
                    group.clone(),
                    hidden,
                    IconName::Trash,
                    true,
                    "Delete",
                    move |cx| {
                        let message_id = ask_delete_view.read(cx).state.files[index].message_id;
                        ask_delete_view.update(cx, |this, cx| {
                            this.state.pending_delete = Some(message_id);
                            this.state.selected_file = Some(index);
                            cx.notify();
                        });
                    },
                )
            });

        div()
            .id(SharedString::from(format!("row-{index}")))
            .group(group)
            .flex()
            .flex_row()
            .items_center()
            .h(px(theme::ROW_H))
            .px(px(theme::ROW_PAD))
            .gap(px(theme::COL_GAP))
            .cursor_pointer()
            .when(selected, |el| el.bg(theme::overlay_accent_1()))
            .hover(|style| {
                if selected {
                    style.bg(theme::overlay_accent_2())
                } else {
                    style.bg(theme::overlay_2())
                }
            })
            .on_click({
                let clicking = open_view.clone();
                move |_, _window, cx| {
                    clicking.update(cx, |this, cx| {
                        this.state.selected_file = Some(index);
                        this.state.pending_delete = None;
                        cx.notify();
                    });
                }
            })
            .child(
                div()
                    .flex_1()
                    .flex_grow(2.)
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(file_chip(file.kind))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::TYPE_BASE))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text())
                            .child(file.name.clone()),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(96.))
                    .truncate()
                    .text_size(px(theme::TYPE_SM))
                    .text_color(theme::text_muted())
                    .child(date),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(84.))
                    .truncate()
                    .text_size(px(theme::TYPE_SM))
                    .text_color(theme::text_muted())
                    .child(size),
            )
            .child(actions)
            .into_any_element()
    }

    /// A row in the trash. It is a record of something deleted rather than a
    /// live item, so it is not clickable and its Restore / Delete permanently
    /// pair is on the row at all times, as the original's `actionsInline` rows
    /// are.
    fn render_trash_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        let entry = self.state.trash[index].clone();
        let file = &entry.file;
        let confirming = self.state.pending_purge == Some(index);
        let group = SharedString::from(format!("trash-row-{index}"));

        let restore_view = view.clone();
        let ask_purge_view = view.clone();
        let purge_view = view.clone();
        let cancel_view = view.clone();

        let (countdown, urgent) = purge_countdown(Some(entry.purge_after()), Utc::now());
        let origin = entry_origin(&entry.origin, file.size, file.kind != FileKind::Folder);
        let size = if file.kind == FileKind::Folder {
            "—".to_string()
        } else {
            human_size(file.size)
        };

        let actions = div()
            .w(px(theme::COL_ACTIONS_W))
            .flex_shrink_0()
            .flex()
            .flex_row()
            .items_center()
            .justify_end()
            .gap(px(4.))
            .child(if confirming {
                row_action(
                    SharedString::from(format!("purge-confirm-{index}")),
                    group.clone(),
                    false,
                    IconName::Check,
                    true,
                    "Delete permanently",
                    move |cx| purge_trash_entry(purge_view.clone(), index, cx),
                )
            } else {
                row_action(
                    SharedString::from(format!("restore-{index}")),
                    group.clone(),
                    false,
                    IconName::Undo2,
                    false,
                    "Restore",
                    move |cx| restore_trash_entry(restore_view.clone(), index, cx),
                )
            })
            .child(if confirming {
                row_action(
                    SharedString::from(format!("purge-cancel-{index}")),
                    group.clone(),
                    false,
                    IconName::X,
                    false,
                    "Cancel",
                    move |cx| {
                        cancel_view.update(cx, |this, cx| {
                            this.state.pending_purge = None;
                            cx.notify();
                        });
                    },
                )
            } else {
                row_action(
                    SharedString::from(format!("purge-{index}")),
                    group.clone(),
                    false,
                    IconName::Trash,
                    true,
                    "Delete permanently",
                    move |cx| {
                        ask_purge_view.update(cx, |this, cx| {
                            this.state.pending_purge = Some(index);
                            cx.notify();
                        });
                    },
                )
            });

        div()
            .id(SharedString::from(format!("trash-row-{index}")))
            .group(group)
            .role(gpui::Role::Row)
            .aria_label(format!("{} — deleted from {origin}", file.name))
            .flex()
            .flex_row()
            .items_center()
            .h(px(theme::ROW_H))
            .px(px(theme::ROW_PAD))
            .gap(px(theme::COL_GAP))
            .child(
                div()
                    .flex_1()
                    .flex_grow(2.)
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(file_chip(file.kind))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::TYPE_BASE))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text_soft())
                            .child(file.name.clone()),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(96.))
                    .truncate()
                    .text_size(px(theme::TYPE_SM))
                    .text_color(if urgent {
                        theme::warning()
                    } else {
                        theme::text_muted()
                    })
                    .child(countdown),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(84.))
                    .truncate()
                    .text_size(px(theme::TYPE_SM))
                    .text_color(theme::text_muted())
                    .child(size),
            )
            .child(actions)
            .into_any_element()
    }
}

// --- Header pieces ----------------------------------------------------------

fn sort_button(
    view: Entity<TDriveView>,
    key: SortKey,
    label: &'static str,
    sort: SortState,
) -> AnyElement {
    let active = sort.is_active(key);
    div()
        .id(SharedString::from(format!("sort-{label}")))
        .role(gpui::Role::Button)
        .aria_label(format!("Sort by {label}"))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(6.))
        .cursor_pointer()
        .when(active, |el| {
            el.bg(theme::overlay_white_1())
                .text_color(theme::text_soft())
        })
        .hover(|style| {
            style
                .bg(theme::overlay_white_1())
                .text_color(theme::text_soft())
        })
        .on_click(move |_, _window, cx| {
            view.update(cx, |this, cx| {
                this.state.sort.toggle(key);
                cx.notify();
            });
        })
        .child(label)
        .child(
            div()
                .w(px(10.))
                .flex()
                .justify_center()
                .children(active.then(|| {
                    glyph(
                        if sort.ascending {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        },
                        12.,
                        theme::accent(),
                    )
                })),
        )
        .into_any_element()
}

/// "Empty trash". Destructive and irreversible, so the first click only arms
/// it; the second empties. With nothing to empty the control is absent rather
/// than disabled, as in the original shell.
fn empty_trash_button(view: Entity<TDriveView>, armed: bool) -> AnyElement {
    let label = if armed {
        "Delete everything?"
    } else {
        "Empty trash"
    };
    let button_view = view.clone();
    div()
        .id("trash-empty-btn")
        .role(gpui::Role::Button)
        .aria_label("Empty trash")
        .flex_shrink_0()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .px(px(12.))
        .py(px(6.))
        .rounded(px(6.))
        .border_1()
        .border_color(if armed {
            theme::danger()
        } else {
            theme::overlay_danger_2()
        })
        .when(armed, |el| el.bg(theme::overlay_danger_1()))
        .cursor_pointer()
        .text_size(px(theme::TYPE_SM))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::danger())
        .hover(|style| {
            style
                .bg(theme::overlay_danger_1())
                .border_color(theme::danger())
        })
        .on_click(move |_, _window, cx| {
            button_view.update(cx, |this, cx| {
                if this.state.confirm_empty_trash {
                    let count = this.state.trash.len();
                    this.state.trash.clear();
                    this.state.confirm_empty_trash = false;
                    this.state.pending_purge = None;
                    this.state.status = format!("Emptied the trash ({count} items)");
                } else {
                    this.state.confirm_empty_trash = true;
                }
                cx.notify();
            });
        })
        .child(glyph(IconName::Trash, 15., theme::danger()))
        .child(label)
        .into_any_element()
}

fn skeleton_row(index: usize) -> AnyElement {
    let bar = move |width: f32| {
        div()
            .h(px(10.))
            .w(px(width))
            .rounded(px(999.))
            .bg(theme::surface_control())
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .h(px(theme::ROW_H))
        .px(px(theme::ROW_PAD))
        .gap(px(theme::COL_GAP))
        .child(
            div()
                .flex_1()
                .flex_grow(2.)
                .min_w_0()
                .flex()
                .flex_row()
                .items_center()
                .child(
                    div()
                        .w(px(theme::TILE))
                        .h(px(theme::TILE))
                        .mr(px(theme::COL_GAP))
                        .flex_shrink_0()
                        .rounded(px(6.))
                        .bg(theme::surface_control()),
                )
                .child(bar(160. + (index as f32) * 14.)),
        )
        .child(div().flex_1().min_w(px(96.)).flex().child(bar(58.)))
        .child(div().flex_1().min_w(px(84.)).flex().child(bar(44.)))
        .child(
            div()
                .w(px(theme::COL_ACTIONS_W))
                .flex_shrink_0()
                .flex()
                .justify_end()
                .child(bar(28.)),
        )
        .into_any_element()
}

fn drive_rows(state: &AppState, kind: DialogKind) -> Vec<(usize, String)> {
    state
        .dialogs
        .iter()
        .enumerate()
        .filter(|(_, d)| d.kind == kind)
        .map(|(i, d)| (i, d.title.clone()))
        .collect()
}

// --- Login pieces -----------------------------------------------------------

fn text_field(state: &Entity<InputState>) -> impl IntoElement {
    Input::new(state)
}

fn field(label: &'static str, input: impl IntoElement) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(theme::TYPE_XS))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text_muted())
                .child(label),
        )
        .child(input)
        .into_any_element()
}

enum LoginAction {
    Connect,
    SendCode,
    Verify,
    Unlock,
}

fn login_button(
    id: &'static str,
    label: &'static str,
    view: Entity<TDriveView>,
    action: LoginAction,
) -> Button {
    Button::new(id)
        .primary()
        .label(label)
        .on_click(move |_, _window, cx| {
            run_login_action(view.clone(), &action, cx);
        })
}

fn run_login_action(view: Entity<TDriveView>, action: &LoginAction, cx: &mut App) {
    match action {
        LoginAction::Connect => connect_telegram(view, cx),
        LoginAction::SendCode => send_login_code(view, cx),
        LoginAction::Verify => verify_login_code(view, cx),
        LoginAction::Unlock => unlock_two_factor(view, cx),
    }
}

fn connect_telegram(view: Entity<TDriveView>, cx: &mut App) {
    let (api_id_raw, api_hash_raw, session_path) = {
        let this = view.read(cx);
        (
            this.api_id_input.read(cx).value().to_string(),
            this.api_hash_input.read(cx).value().to_string(),
            this.state.session_path.clone(),
        )
    };

    let Ok(api_id) = api_id_raw.trim().parse::<i32>() else {
        set_login_error(view, "API ID must be a number.", cx);
        return;
    };
    let api_hash = api_hash_raw.trim().to_string();
    if api_hash.is_empty() {
        set_login_error(view, "API hash is required.", cx);
        return;
    }

    let creds = crate::models::Credentials { api_id, api_hash };
    view.update(cx, |this, cx| {
        this.state.creds = Some(creds.clone());
        this.state.login.error = None;
        this.state.login.working = Some("Connecting to Telegram...".into());
        cx.notify();
    });

    cx.spawn(async move |cx| {
        let session = session_path.clone();
        let creds_for_task = creds.clone();
        let result = cx
            .background_spawn(async move { tele::connect(&creds_for_task, session) })
            .await;

        let (client, error) = match result {
            Ok(client) => (Some(client), None),
            Err(message) => (None, Some(message)),
        };

        let authorized = if let Some(client) = &client {
            let probe = client.clone();
            cx.background_spawn(async move { tele::is_authorized(&probe) })
                .await
        } else {
            false
        };

        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.state.login.working = None;
                match (&client, error) {
                    (Some(client), _) => {
                        this.state.tele = Some(client.clone());
                        this.state.login.step = if authorized {
                            LoginStep::LoggedIn
                        } else {
                            LoginStep::NeedPhone
                        };
                    }
                    (None, Some(message)) => this.state.login.error = Some(message),
                    _ => {}
                }
                cx.notify();
            });
        });

        if authorized {
            cx.update(|cx| {
                let client = view.read(cx).state.tele.clone();
                if let Some(client) = client {
                    spawn_load_dialogs_inner(view.clone(), client, cx);
                }
            });
        }
    })
    .detach();
}

fn set_login_error(view: Entity<TDriveView>, message: &str, cx: &mut App) {
    view.update(cx, |this, cx| {
        this.state.login.error = Some(message.to_string());
        cx.notify();
    });
}

fn send_login_code(view: Entity<TDriveView>, cx: &mut App) {
    let phone = view.read(cx).phone_input.read(cx).value().to_string();
    let (client, api_hash) = {
        let this = view.read(cx);
        let Some(client) = this.state.tele.clone() else {
            return;
        };
        let api_hash = this
            .state
            .creds
            .as_ref()
            .map(|c| c.api_hash.clone())
            .unwrap_or_default();
        (client, api_hash)
    };

    view.update(cx, |this, cx| {
        this.state.login.working = Some("Requesting a login code...".into());
        this.state.login.error = None;
        cx.notify();
    });

    cx.spawn(async move |cx| {
        let phone_for_task = phone.clone();
        let result = cx
            .background_spawn(
                async move { tele::request_code(&client, &phone_for_task, &api_hash) },
            )
            .await;
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.state.login.working = None;
                match result {
                    Ok(token) => {
                        this.state.login.phone = phone.clone();
                        this.state.login.login_token = Some(token);
                        this.state.login.step = LoginStep::NeedCode;
                    }
                    Err(message) => this.state.login.error = Some(message),
                }
                cx.notify();
            });
        });
    })
    .detach();
}

fn verify_login_code(view: Entity<TDriveView>, cx: &mut App) {
    let code = view.read(cx).code_input.read(cx).value().to_string();
    let grabbed = view.update(cx, |this, _cx| {
        (this.state.tele.clone(), this.state.login.login_token.take())
    });
    let (Some(client), Some(token)) = grabbed else {
        set_login_error(view, "Request a new code.", cx);
        return;
    };

    view.update(cx, |this, cx| {
        this.state.login.working = Some("Checking the code...".into());
        cx.notify();
    });

    cx.spawn(async move |cx| {
        let result = cx
            .background_spawn(async move { tele::sign_in(&client, token, &code) })
            .await;
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.state.login.working = None;
                match result {
                    Ok(None) => {
                        this.state.login.step = LoginStep::LoggedIn;
                        if let Some(client) = this.state.tele.clone() {
                            spawn_load_dialogs_inner(view.clone(), client, cx);
                        }
                    }
                    Ok(Some(token)) => {
                        this.state.login.password_token = Some(token);
                        this.state.login.step = LoginStep::NeedPassword;
                    }
                    Err(message) => this.state.login.error = Some(message),
                }
                cx.notify();
            });
        });
    })
    .detach();
}

fn unlock_two_factor(view: Entity<TDriveView>, cx: &mut App) {
    let password = view.read(cx).password_input.read(cx).value().to_string();
    let grabbed = view.update(cx, |this, _cx| {
        (
            this.state.tele.clone(),
            this.state.login.password_token.take(),
        )
    });
    let (Some(client), Some(token)) = grabbed else {
        set_login_error(view, "Request a new login code.", cx);
        return;
    };

    view.update(cx, |this, cx| {
        this.state.login.working = Some("Verifying the password...".into());
        cx.notify();
    });

    cx.spawn(async move |cx| {
        let result = cx
            .background_spawn(async move { tele::check_password(&client, token, &password) })
            .await;
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.state.login.working = None;
                match result {
                    Ok(_) => {
                        this.state.login.step = LoginStep::LoggedIn;
                        if let Some(client) = this.state.tele.clone() {
                            spawn_load_dialogs_inner(view.clone(), client, cx);
                        }
                    }
                    Err(message) => this.state.login.error = Some(message),
                }
                cx.notify();
            });
        });
    })
    .detach();
}

// --- Data actions -----------------------------------------------------------

fn open_drive_clicked(view: Entity<TDriveView>, index: usize, cx: &mut App) {
    spawn_open_drive_inner(view, index, cx);
}

fn spawn_load_dialogs_inner(view: Entity<TDriveView>, client: TeleClient, cx: &mut App) {
    view.update(cx, |this, cx| {
        this.state.dialogs_loading = true;
        this.state.error = None;
        cx.notify();
    });
    cx.spawn(async move |cx| {
        let result = cx
            .background_spawn({
                let client = client.clone();
                async move { tele::list_dialogs(&client) }
            })
            .await;
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.state.dialogs_loading = false;
                match result {
                    Ok(dialogs) => {
                        this.state.dialogs = dialogs;
                        this.state.status = format!("{} drives", this.state.dialogs.len());
                        let first_personal =
                            this.state.dialogs.iter().position(|d| d.kind.is_personal());
                        if let Some(index) = first_personal {
                            spawn_open_drive_inner(view.clone(), index, cx);
                        }
                    }
                    Err(message) => this.state.error = Some(message),
                }
                cx.notify();
            });
        });
    })
    .detach();
}

/// Load a drive's files. Telegram has no notion of cancelling a request, so a
/// reply that arrives after the user has moved to another drive is dropped
/// rather than allowed to overwrite the list they are now looking at.
fn spawn_open_drive_inner(view: Entity<TDriveView>, index: usize, cx: &mut App) {
    let (client, peer_ref, title, dialog_id) = {
        let this = view.read(cx);
        let Some(client) = this.state.tele.clone() else {
            return;
        };
        let Some(dialog) = this.state.dialogs.get(index) else {
            return;
        };
        (client, dialog.peer_ref, dialog.title.clone(), dialog.id)
    };

    view.update(cx, |this, cx| {
        this.state.active_drive = Some(index);
        this.state.view = ViewMode::Drive;
        this.state.selected_file = None;
        this.state.pending_delete = None;
        this.state.pending_purge = None;
        this.state.confirm_empty_trash = false;
        this.state.files = Vec::new();
        this.state.files_loading = true;
        this.state.status = format!("Opening {title}...");
        this.state.error = None;
        cx.notify();
    });

    cx.spawn(async move |cx| {
        let result = cx
            .background_spawn({
                let client = client.clone();
                async move { tele::list_files(&client, peer_ref) }
            })
            .await;
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                // Still the drive this request was started for?
                if this.state.active_dialog().map(|d| d.id) != Some(dialog_id) {
                    return;
                }
                this.state.files_loading = false;
                match result {
                    Ok(files) => {
                        this.state.status = format!("{} files in {title}", files.len());
                        this.state.files = files;
                    }
                    Err(message) => {
                        this.state.status = "Could not open drive".to_string();
                        this.state.error = Some(message);
                    }
                }
                cx.notify();
            });
        });
    })
    .detach();
}

fn spawn_download(view: Entity<TDriveView>, index: usize, cx: &mut App) {
    let (client, peer_ref, dialog_id, message_id, name) = {
        let this = view.read(cx);
        let Some(dialog) = this.state.active_dialog().cloned() else {
            return;
        };
        let Some(file) = this.state.files.get(index) else {
            return;
        };
        (
            this.state.tele.clone(),
            dialog.peer_ref,
            dialog.id,
            file.message_id,
            sanitize_name(&file.name),
        )
    };

    // A download has to come from Telegram; without a session there is nothing
    // to fetch, so say so rather than leaving the row looking inert.
    let Some(client) = client else {
        view.update(cx, |this, cx| {
            this.state.status = "Connect to Telegram to download".to_string();
            cx.notify();
        });
        return;
    };

    let dest = download_dir().join(name);
    view.update(cx, |this, cx| {
        if let Some(file) = this.state.files.get_mut(index) {
            file.local_state = LocalState::Downloading { done: 0 };
        }
        this.state.error = None;
        cx.notify();
    });

    cx.spawn(async move |cx| {
        let dest_for_task = dest.clone();
        let result = cx
            .background_spawn(async move {
                tele::download_file(&client, peer_ref, message_id, &dest_for_task)
            })
            .await;
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                // A drive switch replaces the whole list, so the row this
                // download belongs to is only still there if the drive is.
                if this.state.active_dialog().map(|d| d.id) != Some(dialog_id) {
                    return;
                }
                if let Some(file) = this.state.files.get_mut(index) {
                    file.local_state = match &result {
                        Ok(()) => LocalState::Downloaded(dest.clone()),
                        Err(message) => LocalState::Failed(truncate(message, 40)),
                    };
                }
                this.state.status = match &result {
                    Ok(()) => format!("Saved to {}", dest.display()),
                    Err(message) => format!("Download failed: {}", truncate(message, 80)),
                };
                cx.notify();
            });
        });
    })
    .detach();
}

fn spawn_upload(view: Entity<TDriveView>, cx: &mut App) {
    let (client, peer_ref) = {
        let this = view.read(cx);
        let (Some(client), Some(dialog)) = (this.state.tele.clone(), this.state.active_dialog())
        else {
            // An upload has nothing to write to without a session, so say so
            // rather than leaving the button looking inert.
            view.update(cx, |this, cx| {
                this.state.status = "Connect to Telegram to upload".to_string();
                cx.notify();
            });
            return;
        };
        (client, dialog.peer_ref)
    };

    view.update(cx, |this, cx| {
        this.state.status = "Choosing a file...".to_string();
        this.state.error = None;
        cx.notify();
    });

    cx.spawn(async move |cx| {
        let picked = cx
            .background_spawn(async move { rfd::FileDialog::new().pick_file() })
            .await;
        let Some(path) = picked else {
            cx.update(|cx| {
                view.update(cx, |this, cx| {
                    this.state.status = "Upload cancelled".to_string();
                    cx.notify();
                });
            });
            return;
        };
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.state.status = format!("Uploading {}...", path.display());
                cx.notify();
            });
        });
        let label = path.display().to_string();
        let result = cx
            .background_spawn(async move { tele::upload_file(&client, peer_ref, &path) })
            .await;
        cx.update(|cx| {
            view.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.state.status = format!("Uploaded {label}");
                    if let Some(index) = this.state.active_drive {
                        spawn_open_drive_inner(view.clone(), index, cx);
                    }
                    cx.notify();
                }
                Err(message) => {
                    this.state.error = Some(message);
                    cx.notify();
                }
            });
        });
    })
    .detach();
}

/// Move a file out of the live list and into the trash under a known origin.
fn move_file_to_trash(state: &mut AppState, message_id: i32, origin: &str) -> Option<String> {
    let pos = state
        .files
        .iter()
        .position(|f| f.message_id == message_id)?;
    let entry = state.trash_file(pos, origin)?;
    state.selected_file = None;
    Some(entry.file.name)
}

fn spawn_delete(view: Entity<TDriveView>, index: usize, cx: &mut App) {
    let (client, peer_ref, dialog_id, title, message_id, name, demo) = {
        let this = view.read(cx);
        let Some(dialog) = this.state.active_dialog().cloned() else {
            return;
        };
        let Some(file) = this.state.files.get(index) else {
            return;
        };
        (
            this.state.tele.clone(),
            dialog.peer_ref,
            dialog.id,
            dialog.title.clone(),
            file.message_id,
            file.name.clone(),
            this.state.demo,
        )
    };

    view.update(cx, |this, cx| {
        this.state.pending_delete = None;
        this.state.status = "Deleting...".to_string();
        this.state.error = None;
        cx.notify();
    });

    // Without a session there is nothing to delete on Telegram, but the demo
    // still walks the local half of the flow so the trash is reachable.
    let Some(client) = client else {
        if demo {
            view.update(cx, |this, cx| {
                this.state.status = match move_file_to_trash(&mut this.state, message_id, &title) {
                    Some(name) => format!("Moved {name} to trash"),
                    None => "Nothing to delete".to_string(),
                };
                cx.notify();
            });
        }
        return;
    };

    cx.spawn(async move |cx| {
        let result = cx
            .background_spawn(async move { tele::delete_file(&client, peer_ref, message_id) })
            .await;
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                // The row may have moved, or the user may have moved to another
                // drive, while the request was in flight.
                if this.state.active_dialog().map(|d| d.id) != Some(dialog_id) {
                    return;
                }
                match result {
                    Ok(()) => {
                        // Deletion is a move to the trash, so the file keeps its
                        // name and size there until it is purged.
                        move_file_to_trash(&mut this.state, message_id, &title);
                        this.state.status = format!("Moved {name} to trash");
                    }
                    Err(message) => this.state.error = Some(message),
                }
                cx.notify();
            });
        });
    })
    .detach();
}

/// Put a trashed item back in the drive it came from.
///
/// This port's trash is session-scoped: nothing was sent to Telegram to move
/// the item out of the drive, so restoring is a local move back into the list.
fn restore_trash_entry(view: Entity<TDriveView>, index: usize, cx: &mut App) {
    view.update(cx, |this, cx| {
        if index < this.state.trash.len() {
            let entry = this.state.trash.remove(index);
            this.state.status = format!("Restored {}", entry.file.name);
            this.state.files.push(entry.file);
        }
        this.state.pending_purge = None;
        cx.notify();
    });
}

/// Drop a trashed item for good.
fn purge_trash_entry(view: Entity<TDriveView>, index: usize, cx: &mut App) {
    view.update(cx, |this, cx| {
        if index < this.state.trash.len() {
            let entry = this.state.trash.remove(index);
            this.state.status = format!("Deleted {} permanently", entry.file.name);
        }
        this.state.pending_purge = None;
        cx.notify();
    });
}

fn download_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|dirs| dirs.download_dir().map(|path| path.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn sanitize_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| if std::path::is_separator(c) { '_' } else { c })
        .collect();
    if clean.trim().is_empty() {
        "download".to_string()
    } else {
        clean
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[allow(dead_code)]
fn unused(file: &RemoteFile) -> String {
    file.name.clone()
}
