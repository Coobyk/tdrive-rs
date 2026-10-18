//! The TDrive desktop shell.
//!
//! This mirrors the Svelte dashboard in `frontend/src/ui/AppShell.svelte` and
//! its stylesheets: a 240px drive sidebar, a header with the search field and
//! the upload action, a breadcrumb, and the sortable Name / Date / Size /
//! Actions file table. Colours come from [`crate::theme`], which carries the
//! original palette, so nothing renders light-on-light.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use chrono::{DateTime, Utc};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, FontWeight, IntoElement, ParentElement, Render, Rgba,
    SharedString, Styled, Subscription, Window, div, px, uniform_list,
};
use gpui_component::{
    Disableable, Icon,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
};
use gpui_kit_assets::IconName;

use crate::config::{self, DEFAULT_CHANNEL_ABOUT, DEFAULT_CHANNEL_TITLE, DriveConfig, StoredDrive};
use crate::crypto;
use crate::format::{entry_origin, human_size, purge_countdown, short_date};
use crate::models::{
    AppState, ContentRef, Drive, DriveCandidate, DriveKind, FileKind, LocalState, LoginStep,
    RemoteFile, SortKey, SortState, TrashEntry, ViewMode,
};
use crate::tele;
use crate::theme;

type TeleClient = grammers_client::Client;

/// The ordered row indices a frame draws, remembered until something that can
/// change them changes.
///
/// The original memoises the same thing for the same reason: a folder can hold
/// six figures of entries, and re-filtering and re-sorting all of them on every
/// scroll tick would be visible.
struct RowsCache {
    version: u64,
    view: ViewMode,
    drive: Option<usize>,
    folder: String,
    trash: bool,
    rows: Rc<Vec<usize>>,
    /// The drive's stored bytes at this version. Summing every file on each
    /// redraw would put the one remaining O(files) walk back into the frame
    /// that the row window exists to avoid, so it is memoised with the rows.
    total_size: i64,
}

pub struct TDriveView {
    state: AppState,
    api_id_input: Entity<InputState>,
    api_hash_input: Entity<InputState>,
    phone_input: Entity<InputState>,
    code_input: Entity<InputState>,
    password_input: Entity<InputState>,
    search_input: Entity<InputState>,
    /// The passphrase field in the "Unlock encrypted files" prompt.
    vault_input: Entity<InputState>,
    rows_cache: RefCell<Option<RowsCache>>,
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
        let vault_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Encryption password"));

        // Secrets render as dots and stay out of the clipboard, the way the
        // original treats the two-factor password and the passphrase.
        password_input.update(cx, |state, cx| state.set_masked(true, window, cx));
        vault_input.update(cx, |state, cx| state.set_masked(true, window, cx));

        let mut this = Self {
            state: AppState::default(),
            api_id_input,
            api_hash_input,
            phone_input,
            code_input,
            password_input,
            search_input,
            vault_input,
            rows_cache: RefCell::new(None),
            _subscriptions: Vec::new(),
        };

        // A development affordance: fills the shell with representative data so
        // the dashboard can be built, run and screenshotted without a live
        // Telegram session.
        if std::env::var("TDRIVE_DEMO").is_ok() {
            this.state.seed_demo();
        } else if let Some(creds) = config::load_credentials() {
            // The setup screen arrives filled in, because the credentials were
            // already given to this machine, exactly as the original's
            // `imp_config.json` remembers them.
            this.api_id_input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("API ID (from my.telegram.org)")
                    .default_value(creds.api_id.to_string())
            });
            this.api_hash_input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("API hash")
                    .default_value(creds.api_hash.clone())
            });
        }

        let subscription = cx.subscribe(
            &this.search_input,
            |this: &mut Self, _emitter, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = this.search_input.read(cx).value().to_string();
                    this.state.set_query(query);
                    cx.notify();
                }
            },
        );
        this._subscriptions.push(subscription);

        // Enter submits, as it does in every form the original draws: the same
        // key advances each sign-in step and unlocks the vault. The action is
        // deferred because this handler already runs inside this view's update,
        // and starting one of these tasks here would lease the view twice.
        let weak = cx.weak_entity();
        for (input, action) in [
            (this.api_id_input.clone(), LoginAction::Connect),
            (this.api_hash_input.clone(), LoginAction::Connect),
            (this.phone_input.clone(), LoginAction::SendCode),
            (this.code_input.clone(), LoginAction::Verify),
            (this.password_input.clone(), LoginAction::Unlock),
            (this.vault_input.clone(), LoginAction::UnlockVault),
        ] {
            let weak = weak.clone();
            let subscription = cx.subscribe(
                &input,
                move |_this: &mut Self, _emitter, event: &InputEvent, cx| {
                    let submits = matches!(
                        event,
                        InputEvent::PressEnter {
                            secondary: false,
                            shift: false
                        }
                    );
                    if !submits {
                        return;
                    }
                    let Some(view) = weak.upgrade() else {
                        return;
                    };
                    cx.defer(move |cx| run_login_action(view, action, cx));
                },
            );
            this._subscriptions.push(subscription);
        }
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

/// A failed action, shown inside a card rather than as a status line so it is
/// hard to miss and impossible to confuse with progress.
fn error_banner(message: String) -> AnyElement {
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
        .into_any_element()
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
        let content = match self.state.login.step {
            LoginStep::LoggedIn => self.render_app(cx).into_any_element(),
            // Signed in but nothing saved on this device yet: the original asks
            // which channel is the drive before it shows any files.
            LoginStep::ChooseDrive => self.render_drive_picker(cx),
            _ => self.render_login(cx).into_any_element(),
        };
        let vault_prompt = self.render_vault_prompt(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .relative()
            .bg(theme::canvas())
            .text_color(theme::text_soft())
            .child(content)
            .children(vault_prompt)
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
            LoginStep::ChooseDrive => ("", "", Vec::new()),
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
            LoginStep::ChooseDrive => Button::new("noop").ghost(),
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

    /// The "Choose your TDrive" step, shown when no drive is saved on this
    /// device yet.
    ///
    /// Only the account's own broadcast channels appear here, because only a
    /// broadcast channel the account created can hold a drive. Private chats,
    /// groups and channels the account merely follows are filtered out in the
    /// transport, so this screen cannot offer a chat that is not a drive.
    fn render_drive_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        let loading = self.state.candidates_loading;
        let working = self.state.selecting_drive;
        let error = self.state.candidates_error.clone();
        let selected = self.state.selected_candidate;

        let candidates: Vec<AnyElement> = self
            .state
            .candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                let row_view = view.clone();
                let active = selected == Some(index);
                let meta = {
                    let created = chrono::DateTime::from_timestamp(candidate.created, 0);
                    // The original's order: whether the channel is in use, then
                    // when it was created, then its id.
                    let activity = if candidate.has_activity {
                        "In use"
                    } else {
                        "Empty"
                    };
                    format!(
                        "{} · Created {} · ID {}",
                        activity,
                        short_date(created),
                        candidate.id
                    )
                };
                let title = if candidate.title.trim().is_empty() {
                    "Untitled channel".to_string()
                } else {
                    candidate.title.clone()
                };
                div()
                    .id(SharedString::from(format!("candidate-{index}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.))
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(if active {
                        theme::accent()
                    } else {
                        theme::border_soft()
                    })
                    .bg(if active {
                        theme::overlay_accent_1()
                    } else {
                        theme::surface_2()
                    })
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::overlay_2()))
                    .on_click(move |_, _window, cx| {
                        select_candidate(row_view.clone(), index, cx);
                    })
                    .child(
                        div()
                            .w(px(16.))
                            .h(px(16.))
                            .flex_shrink_0()
                            .rounded(px(999.))
                            .border_1()
                            .border_color(if active {
                                theme::accent()
                            } else {
                                theme::border()
                            })
                            .when(active, |el| el.bg(theme::accent())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(theme::TYPE_BASE))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::text())
                                    .child(title),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(theme::TYPE_XS))
                                    .text_color(theme::text_muted())
                                    .child(meta),
                            ),
                    )
                    .children(candidate.recommended.then(|| {
                        div()
                            .flex_shrink_0()
                            .px(px(8.))
                            .py(px(2.))
                            .rounded(px(999.))
                            .bg(theme::overlay_accent_1())
                            .text_size(px(theme::TYPE_XS))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::accent())
                            .child("Recommended")
                    }))
                    .into_any_element()
            })
            .collect();

        let continue_view = view.clone();
        let create_view = view.clone();
        let cancel_view = view.clone();
        let confirm_view = view.clone();
        let confirming = self.state.confirming_create;
        let continue_button = Button::new("continue-drive")
            .primary()
            .label(if working { "Recovering…" } else { "Continue" })
            .loading(working)
            .disabled(selected.is_none() || working)
            .on_click(move |_, _window, cx| {
                continue_with_candidate(continue_view.clone(), cx);
            });

        // Creating a drive makes a channel on the account, so it is confirmed
        // first, exactly as the original's two-step button is.
        let create_button = Button::new("create-drive")
            .ghost()
            .label("Create New TDrive")
            .disabled(working)
            .on_click(move |_, _window, cx| {
                request_create_drive(create_view.clone(), cx);
            });
        let confirm_button = Button::new("create-drive-confirm")
            .primary()
            .label("Create")
            .disabled(working)
            .on_click(move |_, _window, cx| {
                create_personal_drive(confirm_view.clone(), cx);
            });
        let cancel_button = Button::new("create-drive-cancel")
            .ghost()
            .label("Cancel")
            .disabled(working)
            .on_click(move |_, _window, cx| {
                cancel_create_drive(cancel_view.clone(), cx);
            });

        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(520.))
                    .flex()
                    .flex_col()
                    .gap(px(18.))
                    .p(px(28.))
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
                                    .child("Choose your TDrive"),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::TYPE_SM))
                                    .text_color(theme::text_muted())
                                    .child(
                                        "No saved drive was found on this device. Pick the \
                                         Telegram channel that holds your files, or start a new \
                                         empty drive.",
                                    ),
                            ),
                    )
                    .children(error.map(error_banner))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .children(loading.then(|| drive_empty("Looking for your drives…")))
                            .children((!loading && candidates.is_empty()).then(|| {
                                drive_empty(
                                    "No channels found. You haven't created any Telegram \
                                     channels, so there is nothing to recover yet.",
                                )
                            }))
                            .children(candidates),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(10.))
                            .child(continue_button)
                            .children((!confirming).then_some(create_button)),
                    )
                    .children(confirming.then(|| {
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap(px(12.))
                            .p(px(12.))
                            .rounded(px(10.))
                            .bg(theme::surface_2())
                            .border_1()
                            .border_color(theme::border_soft())
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .child(
                                        div()
                                            .text_size(px(theme::TYPE_SM))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme::text())
                                            .child("Create a new empty TDrive?"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(theme::TYPE_XS))
                                            .text_color(theme::text_muted())
                                            .child("This creates one new Telegram channel."),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap(px(8.))
                                    .child(cancel_button)
                                    .child(confirm_button),
                            )
                    }))
                    .children(working.then(|| {
                        div()
                            .text_size(px(theme::TYPE_SM))
                            .text_color(theme::text_muted())
                            .child("Preparing your drive…")
                    })),
            )
            .into_any_element()
    }

    /// The prompt an encrypted file raises when the drive's key is not in
    /// memory yet.
    ///
    /// The wording is the original's: it says what the key is for and shows the
    /// hint the drive carries, so a forgotten password is not a dead end.
    fn render_vault_prompt(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let vault = self.state.vault.as_ref()?;
        vault.prompt.as_ref()?;
        let hint = vault.hint.trim().to_string();
        let error = vault.error.clone();
        let working = vault.working;
        let view = cx.entity();
        let unlock_view = view.clone();
        let cancel_view = view.clone();

        let mut unlock = Button::new("vault-unlock").primary().label("Unlock");
        if !working {
            unlock = unlock.on_click(move |_, _window, cx| {
                submit_vault_password(unlock_view.clone(), cx);
            });
        }

        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x05070dcc))
                .child(
                    div()
                        .w(px(390.))
                        .flex()
                        .flex_col()
                        .gap(px(14.))
                        .p(px(24.))
                        .rounded(px(12.))
                        .bg(theme::surface_1())
                        .border_1()
                        .border_color(theme::border_soft())
                        .child(
                            div()
                                .text_size(px(theme::TYPE_MD))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::text())
                                .child("Unlock encrypted files"),
                        )
                        .child(
                            div()
                                .text_size(px(theme::TYPE_SM))
                                .text_color(theme::text_muted())
                                .child(
                                    "This drive stores encrypted files. Enter your encryption \
                                     password to open them.",
                                ),
                        )
                        .children((!hint.is_empty()).then(|| {
                            div()
                                .text_size(px(theme::TYPE_XS))
                                .text_color(theme::text_subtle())
                                .child(format!("Hint: {hint}"))
                        }))
                        .child(text_field(&self.vault_input))
                        .children(error.map(error_banner))
                        .child(
                            div().flex().flex_row().gap(px(10.)).child(unlock).child(
                                Button::new("vault-cancel")
                                    .ghost()
                                    .label("Cancel")
                                    .on_click(move |_, _window, cx| {
                                        cancel_view.update(cx, |this, cx| {
                                            this.state.close_vault_prompt();
                                            cx.notify();
                                        });
                                    }),
                            ),
                        )
                        .children(working.then(|| {
                            div()
                                .text_size(px(theme::TYPE_SM))
                                .text_color(theme::text_muted())
                                .child("Deriving your key…")
                        })),
                )
                .into_any_element(),
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
                drive_rows(state, true),
                drive_rows(state, false),
                state.active_drive,
            )
        };
        let shared_empty = shared.is_empty();
        let view_mode = self.state.view;
        let loading = self.state.drives_loading;
        let status = self.state.status.clone();
        let used = self.drive_size();

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
                                this.state.set_view(ViewMode::Photos);
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
                                this.state.set_view(ViewMode::Trash);
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
                        (!loading && shared_empty).then(|| drive_empty("No shared drives yet")),
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
        let (loading, can_upload, error) = {
            let state = &self.state;
            (
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

        // Back walks one folder up the trail; at the drive root there is
        // nowhere to go, so it dims rather than disappearing.
        let can_go_back = self.state.folder.len() > 1;
        let back_view = view.clone();
        let back = div()
            .id("folder-back")
            .w(px(32.))
            .h(px(32.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .opacity(if can_go_back { 1.0 } else { 0.35 })
            .when(can_go_back, |el| {
                el.cursor_pointer()
                    .hover(|style| style.bg(theme::overlay_2()))
                    .on_click(move |_, _window, cx| {
                        back_view.update(cx, |this, cx| {
                            let target = this.state.folder.len().saturating_sub(2);
                            this.state.go_to_crumb(target);
                            cx.notify();
                        });
                    })
            })
            .child(glyph(IconName::ChevronLeft, 18., theme::text_soft()));

        // The trail is the drive's own path, so a nested folder can be left by
        // clicking any earlier step rather than only by Back.
        let mut trail = div().flex().flex_row().items_center().gap(px(8.)).min_w_0();
        let crumbs = self.state.folder.clone();
        for (index, crumb) in crumbs.iter().enumerate() {
            if index > 0 {
                trail = trail.child(div().text_color(theme::text_muted()).child("/"));
            }
            let last = index + 1 == crumbs.len();
            let crumb_view = view.clone();
            let label = crumb.name.clone();
            trail = trail.child(
                div()
                    .id(SharedString::from(format!("crumb-{index}")))
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::TYPE_BASE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if last {
                        theme::text()
                    } else {
                        theme::text_soft()
                    })
                    .when(!last, |el| {
                        el.cursor_pointer()
                            .hover(|style| style.text_color(theme::accent()))
                            .on_click(move |_, _window, cx| {
                                crumb_view.update(cx, |this, cx| {
                                    this.state.go_to_crumb(index);
                                    cx.notify();
                                });
                            })
                    })
                    .child(label),
            );
        }

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

    /// The row indices the list should draw, remembered until the search box,
    /// the sort, the folder or the contents change.
    ///
    /// Recomputed only when its inputs change, because it is read on every
    /// frame and a folder can hold six figures of entries.
    fn ordered_rows(&self) -> Rc<Vec<usize>> {
        self.row_cache().0
    }

    /// The active drive's stored bytes, memoised with the rows.
    fn drive_size(&self) -> i64 {
        self.row_cache().1
    }

    /// The visible rows and the drive's stored bytes for the current search
    /// box, sort, folder and view, computed once per `rows_version`.
    fn row_cache(&self) -> (Rc<Vec<usize>>, i64) {
        let version = self.state.rows_version;
        let view = self.state.view;
        let drive = self.state.active_drive;
        let folder = self.state.current_folder().to_string();
        let trash = self.state.trash_mode();

        let cached = {
            let cache = self.rows_cache.borrow();
            cache.as_ref().and_then(|cache| {
                let fresh = cache.version == version
                    && cache.view == view
                    && cache.drive == drive
                    && cache.folder == folder
                    && cache.trash == trash;
                fresh.then(|| (Rc::clone(&cache.rows), cache.total_size))
            })
        };
        if let Some(rows) = cached {
            return rows;
        }

        let rows = Rc::new(if trash {
            self.state.visible_trash()
        } else {
            self.state.visible_files()
        });
        let total_size = self.state.total_size();
        *self.rows_cache.borrow_mut() = Some(RowsCache {
            version,
            view,
            drive,
            folder,
            trash,
            rows: Rc::clone(&rows),
            total_size,
        });
        (rows, total_size)
    }

    /// The scrolling list of rows.
    ///
    /// Windowed by height the way the original's `row-window.ts` is: only the
    /// rows the viewport can show are built, and everything off-screen is
    /// spacer. A drive with tens of thousands of files then renders a couple of
    /// dozen rows per frame instead of all of them, which is the difference
    /// between a smooth scroll and a crawl.
    fn render_file_list(&self, loading: bool, cx: &mut Context<Self>) -> AnyElement {
        let trash_mode = self.state.trash_mode();

        if loading {
            return div()
                .id("file-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px(px(10.))
                .flex()
                .flex_col()
                .children((0..7).map(skeleton_row))
                .into_any_element();
        }

        let rows = self.ordered_rows();
        let count = rows.len();
        let view = cx.entity();
        let body = if count == 0 {
            self.empty_state()
        } else {
            let indices = Rc::clone(&rows);
            uniform_list("file-rows", count, move |range, _window, cx| {
                // The closure runs while the frame is laid out, after this
                // view's own render has returned, so reading it here is safe.
                let app: &App = cx;
                let this = view.read(app);
                range
                    .map(|slot| {
                        let index = indices[slot];
                        if trash_mode {
                            this.render_trash_row(index, &view)
                        } else {
                            this.render_file_row(index, &view)
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .flex_1()
            .min_h_0()
            .into_any_element()
        };

        div()
            .id("file-list")
            .flex_1()
            .min_h_0()
            .px(px(10.))
            .flex()
            .flex_col()
            .child(body)
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

    fn render_file_row(&self, index: usize, view: &Entity<TDriveView>) -> AnyElement {
        let view = view.clone();
        let file = self.state.files[index].clone();
        let selected = self.state.selected_file == Some(index);
        let confirming = self.state.pending_delete == Some(file.message_id);
        // A folder opens rather than downloading, so it carries no row actions.
        let is_folder = file.is_folder();
        let folder_id = file.object_id.clone();
        let folder_name = file.name.clone();
        let locked = file.encrypted;
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
        let size = if is_folder {
            "—".to_string()
        } else {
            human_size(file.display_size())
        };
        let meta = match &file.local_state {
            LocalState::Remote => None,
            LocalState::Downloading { .. } => Some(("Downloading…", theme::text_muted())),
            LocalState::Downloaded(_) => Some(("Saved", theme::success())),
            LocalState::Failed(_) => Some(("Failed", theme::danger())),
        };

        let actions = if is_folder {
            // A folder has nothing to download and nothing to delete from here,
            // so it keeps the column's width but no buttons.
            div()
                .w(px(theme::COL_ACTIONS_W))
                .flex_shrink_0()
                .into_any_element()
        } else {
            div()
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
                })
                .into_any_element()
        };

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
                let folder_id = folder_id.clone();
                let folder_name = folder_name.clone();
                move |event, window, cx| {
                    // A single click selects the row; a double click activates
                    // it, which is the split the original's `activateRow`
                    // describes. Activation is what asks for the drive's key,
                    // because an encrypted file cannot be read before it.
                    let activate = event.click_count() >= 2;
                    if is_folder {
                        if activate {
                            clicking.update(cx, |this, cx| {
                                this.state
                                    .enter_folder(folder_id.clone(), folder_name.clone());
                                cx.notify();
                            });
                        }
                        return;
                    }
                    clicking.update(cx, |this, cx| {
                        this.state.selected_file = Some(index);
                        this.state.pending_delete = None;
                        cx.notify();
                    });
                    if activate {
                        spawn_download(clicking.clone(), index, cx);
                        // A row that is locked raises the unlock prompt instead
                        // of downloading, so the caret goes straight into it and
                        // the passphrase can be typed without a second click.
                        focus_unlock_field(&clicking, window, cx);
                    }
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
                    )
                    // A lock says the contents are encrypted, so the row cannot
                    // be read until the drive's key is in memory.
                    .children(locked.then(|| {
                        div().flex_shrink_0().pl(px(6.)).child(glyph(
                            IconName::Lock,
                            13.,
                            theme::text_muted(),
                        ))
                    })),
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
    fn render_trash_row(&self, index: usize, view: &Entity<TDriveView>) -> AnyElement {
        let view = view.clone();
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
                this.state.toggle_sort(key);
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

fn drive_rows(state: &AppState, personal: bool) -> Vec<(usize, String)> {
    state
        .drives
        .iter()
        .enumerate()
        .filter(|(_, drive)| drive.kind.is_personal() == personal)
        .map(|(index, drive)| (index, drive.title.clone()))
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

#[derive(Clone, Copy)]
enum LoginAction {
    Connect,
    SendCode,
    Verify,
    Unlock,
    /// Submit the passphrase in the unlock prompt, which is a form of its own
    /// rather than a step of the sign-in flow.
    UnlockVault,
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
            run_login_action(view.clone(), action, cx);
        })
}

fn run_login_action(view: Entity<TDriveView>, action: LoginAction, cx: &mut App) {
    match action {
        LoginAction::Connect => connect_telegram(view, cx),
        LoginAction::SendCode => send_login_code(view, cx),
        LoginAction::Verify => verify_login_code(view, cx),
        LoginAction::Unlock => unlock_two_factor(view, cx),
        LoginAction::UnlockVault => submit_vault_password(view, cx),
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
        let creds_for_save = creds.clone();
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
                        if !authorized {
                            this.state.login.step = LoginStep::NeedPhone;
                        }
                    }
                    (None, Some(message)) => this.state.login.error = Some(message),
                    _ => {}
                }
                cx.notify();
            });
        });

        // Run the listing after the update above: it updates this same view.
        if authorized {
            if let Some(client) = client {
                let creds = creds_for_save.clone();
                cx.update(|cx| {
                    // Remember the application credentials the way the original
                    // does, so the next run starts filled in.
                    let _ = config::save_credentials(creds.api_id, &creds.api_hash);
                    spawn_after_login(view, client, cx);
                });
            }
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
        // The listing helper updates this same view, so hand it the client out
        // of the closure instead of calling it while the view is leased.
        let to_list = cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.state.login.working = None;
                let client = match result {
                    Ok(None) => {
                        this.state.login.step = LoginStep::LoggedIn;
                        this.state.tele.clone()
                    }
                    Ok(Some(token)) => {
                        this.state.login.password_token = Some(token);
                        this.state.login.step = LoginStep::NeedPassword;
                        None
                    }
                    Err(message) => {
                        this.state.login.error = Some(message);
                        None
                    }
                };
                cx.notify();
                client
            })
        });
        if let Some(client) = to_list {
            cx.update(|cx| spawn_after_login(view, client, cx));
        }
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
        let to_list = cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.state.login.working = None;
                let client = match result {
                    Ok(_) => {
                        this.state.login.step = LoginStep::LoggedIn;
                        this.state.tele.clone()
                    }
                    Err(message) => {
                        this.state.login.error = Some(message);
                        None
                    }
                };
                cx.notify();
                client
            })
        });
        if let Some(client) = to_list {
            cx.update(|cx| spawn_after_login(view, client, cx));
        }
    })
    .detach();
}

// --- Data actions -----------------------------------------------------------

fn open_drive_clicked(view: Entity<TDriveView>, index: usize, cx: &mut App) {
    spawn_open_drive_inner(view, index, cx);
}

/// Turn a drive remembered on disk into a drive the shell can open.
fn drive_from_stored(stored: &StoredDrive) -> Drive {
    Drive {
        id: stored.id,
        title: stored.title.clone(),
        kind: DriveKind::from_str(&stored.kind),
        peer_ref: tele::peer_ref(stored.id, stored.access_hash),
    }
}

/// After sign-in: go straight to the saved drive, or ask which channel is the
/// drive when this device has never been told.
///
/// This is the original's split between `PreparePersonalDrive` and
/// `SelectionRequired`, and it is what keeps the sidebar from filling up with
/// unrelated chats: nothing is listed until a drive is chosen.
///
/// Like the other task helpers here this leases the view, so it must be called
/// from outside a `view.update` closure. GPUI panics if an entity is read while
/// it is being updated, which is why follow-up work is returned out of the
/// closures below instead of being started inside them.
fn spawn_after_login(view: Entity<TDriveView>, client: TeleClient, cx: &mut App) {
    let saved = DriveConfig::load();
    let drives: Vec<Drive> = saved
        .as_ref()
        .map(|config| config.drives.iter().map(drive_from_stored).collect())
        .unwrap_or_default();
    let personal = saved
        .as_ref()
        .and_then(|config| config.personal())
        .and_then(|stored| drives.iter().position(|drive| drive.id == stored.id));

    view.update(cx, |this, cx| {
        this.state.tele = Some(client.clone());
        this.state.drives = drives;
        this.state.drives_loading = false;
        this.state.login.working = None;
        this.state.candidates_loading = personal.is_none();
        this.state.login.step = if personal.is_some() {
            LoginStep::LoggedIn
        } else {
            LoginStep::ChooseDrive
        };
        cx.notify();
    });

    match personal {
        Some(index) => spawn_open_drive_inner(view, index, cx),
        None => spawn_discover_candidates(view, client, cx),
    }
}

/// Offer the account's own broadcast channels as drives.
///
/// The transport only returns channels the account created, so a private chat
/// such as a conversation with a relative can never reach this list.
fn spawn_discover_candidates(view: Entity<TDriveView>, client: TeleClient, cx: &mut App) {
    view.update(cx, |this, cx| {
        this.state.candidates_loading = true;
        this.state.candidates_error = None;
        this.state.confirming_create = false;
        cx.notify();
    });
    cx.spawn(async move |cx| {
        let result = cx
            .background_spawn({
                let client = client.clone();
                async move { tele::list_owned_channels(&client) }
            })
            .await;
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.state.candidates_loading = false;
                match result {
                    Ok(channels) => {
                        let mut candidates: Vec<DriveCandidate> = channels
                            .into_iter()
                            .map(|channel| DriveCandidate {
                                id: channel.id,
                                title: channel.title,
                                created: channel.created,
                                has_activity: channel.has_activity,
                                recommended: false,
                                peer_ref: channel.peer_ref,
                            })
                            .collect();
                        config::order_candidates(
                            &mut candidates,
                            |candidate| candidate.title.as_str(),
                            |candidate| candidate.created,
                            |candidate| candidate.has_activity,
                        );
                        // Only the candidate that leads the list can be
                        // recommended, and only when it carries the title a
                        // drive made by TDrive would have.
                        if let Some(first) = candidates.first_mut() {
                            first.recommended = config::is_default_title(&first.title);
                        }
                        // Nothing is picked for the user: the original leaves
                        // the list unselected so Continue cannot adopt a
                        // channel that was never chosen.
                        this.state.selected_candidate = None;
                        this.state.status = format!("{} channels found", candidates.len());
                        this.state.candidates = candidates;
                    }
                    Err(message) => this.state.candidates_error = Some(message),
                }
                cx.notify();
            });
        });
    })
    .detach();
}

fn select_candidate(view: Entity<TDriveView>, index: usize, cx: &mut App) {
    view.update(cx, |this, cx| {
        this.state.selected_candidate = Some(index);
        cx.notify();
    });
}

/// Ask before creating a channel, because that is a real change to the account
/// rather than a local setting.
fn request_create_drive(view: Entity<TDriveView>, cx: &mut App) {
    view.update(cx, |this, cx| {
        this.state.confirming_create = true;
        this.state.candidates_error = None;
        cx.notify();
    });
}

fn cancel_create_drive(view: Entity<TDriveView>, cx: &mut App) {
    view.update(cx, |this, cx| {
        this.state.confirming_create = false;
        cx.notify();
    });
}

/// Save a channel as this device's personal drive, then open it.
///
/// The channel id and access hash are written to `config.json` in the original's
/// own shape, so the choice survives a restart instead of being asked again.
fn adopt_personal_drive(
    view: Entity<TDriveView>,
    channel_id: i64,
    access_hash: i64,
    title: String,
    peer_ref: grammers_session::types::PeerRef,
    cx: &mut App,
) {
    let mut config = DriveConfig::load().unwrap_or_default();
    config.replace_personal(StoredDrive {
        id: channel_id,
        access_hash,
        title: title.clone(),
        kind: DriveKind::Personal.as_str().to_string(),
    });
    let saved = config.save();
    let mut drives = vec![Drive {
        id: channel_id,
        title: title.clone(),
        kind: DriveKind::Personal,
        peer_ref,
    }];
    for stored in config.drives.iter().filter(|drive| !drive.is_personal()) {
        drives.push(drive_from_stored(stored));
    }

    view.update(cx, |this, cx| {
        this.state.drives = drives;
        this.state.candidates.clear();
        this.state.selected_candidate = None;
        this.state.selecting_drive = false;
        this.state.login.step = LoginStep::LoggedIn;
        this.state.status = match &saved {
            Ok(()) => format!("Using {title}"),
            Err(message) => format!("Using {title} (not saved: {message})"),
        };
        cx.notify();
    });
    spawn_open_drive_inner(view, 0, cx);
}

/// Use the channel the picker has selected.
fn continue_with_candidate(view: Entity<TDriveView>, cx: &mut App) {
    let picked = {
        let this = view.read(cx);
        this.state
            .selected_candidate
            .and_then(|index| this.state.candidates.get(index))
            .map(|candidate| (candidate.id, candidate.title.clone(), candidate.peer_ref))
    };
    let Some((id, title, peer_ref)) = picked else {
        return;
    };
    let access_hash = peer_ref.auth.hash();
    adopt_personal_drive(view, id, access_hash, title, peer_ref, cx);
}

/// Make a brand new channel the drive, for an account that has none yet.
fn create_personal_drive(view: Entity<TDriveView>, cx: &mut App) {
    let client = {
        let this = view.read(cx);
        this.state.tele.clone()
    };
    let Some(client) = client else {
        view.update(cx, |this, cx| {
            this.state.candidates_error = Some("Connect to Telegram first.".to_string());
            cx.notify();
        });
        return;
    };

    view.update(cx, |this, cx| {
        this.state.selecting_drive = true;
        this.state.candidates_error = None;
        cx.notify();
    });

    cx.spawn(async move |cx| {
        let result = cx
            .background_spawn(async move {
                tele::create_personal_channel(&client, DEFAULT_CHANNEL_TITLE, DEFAULT_CHANNEL_ABOUT)
            })
            .await;
        match result {
            Ok(channel) => {
                cx.update(|cx| {
                    adopt_personal_drive(
                        view,
                        channel.id,
                        channel.access_hash,
                        channel.title.clone(),
                        channel.peer_ref,
                        cx,
                    );
                });
            }
            Err(message) => {
                cx.update(|cx| {
                    view.update(cx, |this, cx| {
                        this.state.selecting_drive = false;
                        this.state.confirming_create = false;
                        this.state.candidates_error = Some(message);
                        cx.notify();
                    });
                });
            }
        }
    })
    .detach();
}

/// Load a drive's contents.
///
/// The list comes from the drive's own operation messages, so ordinary chatter
/// in the channel never becomes a row, and each row knows the folder it belongs
/// to. Telegram has no notion of cancelling a request, so a reply that arrives
/// after the user has moved to another drive is dropped rather than allowed to
/// overwrite the list they are now looking at.
///
/// Must not be called from inside a `view.update` closure; interrupt the
/// closure and let it return first.
fn spawn_open_drive_inner(view: Entity<TDriveView>, index: usize, cx: &mut App) {
    let (client, peer_ref, title, drive_id) = {
        let this = view.read(cx);
        let Some(drive) = this.state.drives.get(index) else {
            return;
        };
        (
            this.state.tele.clone(),
            drive.peer_ref,
            drive.title.clone(),
            drive.id,
        )
    };

    // Without a session there is nothing to read; in demo mode the seeded list
    // already stands in for the drive, so switching still works.
    let Some(client) = client else {
        view.update(cx, |this, cx| {
            this.state.open_drive(index);
            this.state.set_view(ViewMode::Drive);
            this.state.files_loading = false;
            this.state.status = format!("{title} (demo data)");
            cx.notify();
        });
        return;
    };

    view.update(cx, |this, cx| {
        this.state.open_drive(index);
        this.state.set_view(ViewMode::Drive);
        this.state.pending_delete = None;
        this.state.pending_purge = None;
        this.state.confirm_empty_trash = false;
        this.state.files.clear();
        this.state.vault = None;
        this.state.files_loading = true;
        this.state.status = format!("Opening {title}...");
        this.state.error = None;
        cx.notify();
    });

    cx.spawn(async move |cx| {
        let result = cx
            .background_spawn({
                let client = client.clone();
                async move { tele::load_drive(&client, peer_ref) }
            })
            .await;
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                // Still the drive this request was started for?
                if this.state.active_dialog().map(|d| d.id) != Some(drive_id) {
                    return;
                }
                this.state.files_loading = false;
                match result {
                    Ok(contents) => {
                        let count = contents.files.len();
                        this.state.files = contents.files;
                        // The drive's own trash is part of what was read, so
                        // the Trash view shows what the *drive* deleted, not
                        // only what this session did.
                        this.state.trash = contents
                            .trash
                            .into_iter()
                            .map(|entry| TrashEntry {
                                deleted_at: DateTime::from_timestamp(entry.deleted_at, 0)
                                    .unwrap_or_else(Utc::now),
                                origin: title.clone(),
                                file: entry.file,
                            })
                            .collect();
                        // The key belongs to the drive, so a drive that has
                        // one asks for it; one that does not never will.
                        this.state.set_vault(contents.vault);
                        this.state.touch();
                        this.state.status = format!("{count} items in {title}");
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

/// Put the caret in the passphrase field when the unlock prompt is up.
///
/// A locked row raises the prompt instead of downloading, and the row's click
/// handler is the only place with the window in hand, so that is where the
/// focus is moved from.
fn focus_unlock_field(view: &Entity<TDriveView>, window: &mut Window, cx: &mut App) {
    let prompting = view
        .read(cx)
        .state
        .vault
        .as_ref()
        .is_some_and(|vault| vault.prompt.is_some());
    if !prompting {
        return;
    }
    let input = view.read(cx).vault_input.clone();
    input.update(cx, |state, cx| state.focus(window, cx));
}

/// The messages a row's bytes live in, in order.
///
/// A plain upload is its own content; a file committed through the
/// writable-mount path names a hidden body message or a multipart group.
fn content_messages(file: &RemoteFile) -> Vec<i32> {
    match &file.content {
        ContentRef::OpMessage => vec![file.message_id],
        ContentRef::Message(id) => vec![*id],
        ContentRef::Parts(ids) => ids.clone(),
    }
}

fn spawn_download(view: Entity<TDriveView>, index: usize, cx: &mut App) {
    let (client, peer_ref, drive_id, message_id, messages, name, encrypted, key) = {
        let this = view.read(cx);
        let Some(drive) = this.state.active_dialog().cloned() else {
            return;
        };
        let Some(file) = this.state.files.get(index) else {
            return;
        };
        let key = this.state.vault.as_ref().and_then(|vault| vault.key);
        (
            this.state.tele.clone(),
            drive.peer_ref,
            drive.id,
            file.message_id,
            content_messages(file),
            sanitize_name(&file.name),
            file.encrypted,
            key,
        )
    };

    // An encrypted file cannot be read without the drive's key, so ask for it
    // first; a successful unlock resumes this download by itself.
    if encrypted && key.is_none() {
        view.update(cx, |this, cx| {
            this.state.request_vault(Some(message_id));
            this.state.status = format!("{name} is encrypted");
            cx.notify();
        });
        return;
    }

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
        // Only an encrypted file takes the decrypting path, and only when the
        // key is actually in memory.
        let key_for_task = if encrypted { key } else { None };
        let result = cx
            .background_spawn(async move {
                tele::download_file(&client, peer_ref, &messages, &dest_for_task, key_for_task)
            })
            .await;
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                // A drive switch replaces the whole list, so the row this
                // download belongs to is only still there if the drive is.
                if this.state.active_dialog().map(|d| d.id) != Some(drive_id) {
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
    let (client, peer_ref, parent) = {
        let this = view.read(cx);
        let (Some(client), Some(drive)) = (this.state.tele.clone(), this.state.active_dialog())
        else {
            // An upload has nothing to write to without a session, so say so
            // rather than leaving the button looking inert.
            view.update(cx, |this, cx| {
                this.state.status = "Connect to Telegram to upload".to_string();
                cx.notify();
            });
            return;
        };
        (
            client,
            drive.peer_ref,
            this.state.current_folder().to_string(),
        )
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
        // A file lands in the folder on screen and is named the way the
        // filesystem names it, because that name goes into the drive's own
        // metadata rather than being Telegram's business.
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "upload".to_string());
        let result = cx
            .background_spawn(
                async move { tele::upload_file(&client, peer_ref, &path, &parent, &name) },
            )
            .await;
        let to_reload = cx.update(|cx| {
            view.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.state.status = format!("Uploaded {label}");
                    cx.notify();
                    this.state.active_drive
                }
                Err(message) => {
                    this.state.error = Some(message);
                    cx.notify();
                    None
                }
            })
        });
        if let Some(index) = to_reload {
            cx.update(|cx| spawn_open_drive_inner(view, index, cx));
        }
    })
    .detach();
}

/// Derive the drive's key from the passphrase the user typed.
///
/// The KDF is deliberately slow, so it runs off the UI thread; a wrong password
/// is reported with the original's wording and the prompt stays open so the
/// attempt can be corrected.
fn submit_vault_password(view: Entity<TDriveView>, cx: &mut App) {
    let password = view.read(cx).vault_input.read(cx).value().to_string();
    let material = {
        let this = view.read(cx);
        let Some(vault) = this.state.vault.as_ref() else {
            return;
        };
        if vault.working {
            return;
        }
        vault.material.clone()
    };
    if password.is_empty() {
        view.update(cx, |this, cx| {
            if let Some(vault) = this.state.vault.as_mut() {
                vault.error = Some("Enter your encryption password.".to_string());
            }
            cx.notify();
        });
        return;
    }

    view.update(cx, |this, cx| {
        if let Some(vault) = this.state.vault.as_mut() {
            vault.working = true;
            vault.error = None;
        }
        cx.notify();
    });

    cx.spawn(async move |cx| {
        let result = cx
            .background_spawn(async move { crypto::unlock(&material, &password) })
            .await;
        let resume = cx.update(|cx| {
            view.update(cx, |this, cx| {
                let Some(vault) = this.state.vault.as_mut() else {
                    return None;
                };
                vault.working = false;
                let resume = match result {
                    Ok(key) => {
                        vault.key = Some(key);
                        vault.error = None;
                        vault.prompt.take().and_then(|prompt| prompt.message_id)
                    }
                    Err(crypto::CryptoError::WrongPassword) => {
                        vault.error = Some("That encryption password is incorrect.".to_string());
                        None
                    }
                    Err(error) => {
                        vault.error = Some(format!("Could not unlock: {error}"));
                        None
                    }
                };
                if this
                    .state
                    .vault
                    .as_ref()
                    .is_some_and(|vault| vault.unlocked())
                {
                    this.state.status = "Encrypted files unlocked".to_string();
                }
                cx.notify();
                resume
            })
        });

        // Unlocking resumes the download the prompt interrupted, if there was
        // one. The row is found by message id so this stays correct even if the
        // list moved underneath it.
        if let Some(message_id) = resume {
            cx.update(|cx| {
                let index = view
                    .read(cx)
                    .state
                    .files
                    .iter()
                    .position(|file| file.message_id == message_id);
                if let Some(index) = index {
                    spawn_download(view, index, cx);
                }
            });
        }
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
            this.state.touch();
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
