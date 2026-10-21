//! The TDrive desktop shell.
//!
//! This mirrors the Svelte dashboard in `frontend/src/ui/AppShell.svelte` and
//! its stylesheets: a 240px drive sidebar, a header with the search field and
//! the upload action, a breadcrumb, and the sortable Name / Date / Size /
//! Actions file table. Colours come from [`crate::theme`], which carries the
//! original palette, so nothing renders light-on-light.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use chrono::{DateTime, Utc};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, FontWeight, IntoElement, ListAlignment, ListState, ObjectFit,
    ParentElement, Render, Rgba, SharedString, Styled, Subscription, Window, div, img, list, px,
    rgb, uniform_list,
};
use gpui_component::{
    Disableable, Icon,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
};
use gpui_kit_assets::IconName;

use crate::theme;
use tdrive_rs::cache;
use tdrive_rs::config::{
    self, DEFAULT_CHANNEL_ABOUT, DEFAULT_CHANNEL_TITLE, DriveConfig, StoredDrive, ThemePreference,
};
use tdrive_rs::crypto::{self, VaultMaterial};
use tdrive_rs::format::{entry_origin, human_size, purge_countdown, short_date};
use tdrive_rs::models::{
    AppState, ContentRef, Crumb, DerivativeJob, Drive, DriveCandidate, DriveKind, FileKind,
    LocalState, LoginStep, PURGE_WINDOW_DAYS, Preview, PreviewKind, RemoteFile, SortKey, SortState,
    TrashEntry, VaultResume, ViewMode, preview_kind,
};
use tdrive_rs::ops::{self, Op};
use tdrive_rs::rendition;
use tdrive_rs::tele;
use tdrive_rs::tele::DriveContents;

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
    /// Where each search result lives, by file index, drawn in place of its
    /// date. Built with the rows, because a parent walk per frame is the same
    /// kind of work the row window exists to avoid.
    labels: Rc<BTreeMap<usize, String>>,
}

pub struct TDriveView {
    state: AppState,
    api_id_input: Entity<InputState>,
    api_hash_input: Entity<InputState>,
    phone_input: Entity<InputState>,
    code_input: Entity<InputState>,
    password_input: Entity<InputState>,
    search_input: Entity<InputState>,
    /// The appearance the user last chose, kept with the drive list so it
    /// survives a restart.
    theme_pref: ThemePreference,
    /// True once the `TDRIVE_UPLOAD` development aid has sent its file.
    upload_hook_done: bool,
    /// Whether the account menu is open.
    menu_open: bool,
    /// What the account menu is showing.
    menu_view: MenuView,
    /// True when the passphrase field of the unlock prompt is waiting for the
    /// keyboard.
    ///
    /// The focus is given while the frame is drawn rather than where the
    /// prompt was opened, because the click that opened it is still finishing
    /// and takes the focus back for itself.
    vault_focus_pending: bool,
    /// The passphrase field in the "Unlock encrypted files" prompt.
    vault_input: Entity<InputState>,
    /// The three fields of the first-time encryption setup: password, its
    /// confirmation, and the optional hint the unlock prompt shows.
    encrypt_password_input: Entity<InputState>,
    encrypt_confirm_input: Entity<InputState>,
    encrypt_hint_input: Entity<InputState>,
    /// The name field in the rename dialog.
    rename_input: Entity<InputState>,
    /// The name field in the "New folder" dialog.
    folder_input: Entity<InputState>,
    rows_cache: RefCell<Option<RowsCache>>,
    /// The variable-height list that virtualizes the gallery. It lives on the
    /// view so measurements survive between frames, which is what lets headers
    /// and tile rows share one scroll surface without the uniform-height
    /// assumption `uniform_list` makes.
    gallery_list: ListState,
    /// The `rows_version` the gallery list was last rebuilt for.
    gallery_list_version: u64,
    /// The window width the last frame laid out with. The gallery sizes its
    /// tiles from it, the way the original's `auto-fill` grid does.
    viewport_w: f32,
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
        let encrypt_password_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Password"));
        let encrypt_confirm_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Confirm password"));
        let encrypt_hint_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Optional hint (do not put the password here)")
        });
        let rename_input = cx.new(|cx| InputState::new(window, cx).placeholder("Name"));
        let folder_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. Invoices"));

        // Secrets render as dots and stay out of the clipboard, the way the
        // original treats the two-factor password and the passphrase.
        password_input.update(cx, |state, cx| state.set_masked(true, window, cx));
        vault_input.update(cx, |state, cx| state.set_masked(true, window, cx));
        encrypt_password_input.update(cx, |state, cx| state.set_masked(true, window, cx));
        encrypt_confirm_input.update(cx, |state, cx| state.set_masked(true, window, cx));

        // The theme the user last chose is the one on screen, before anything
        // is drawn.
        let theme_pref = DriveConfig::load()
            .map(|config| config.theme)
            .unwrap_or_default();
        theme::set_theme(theme_pref.active());
        // The controls the port borrows follow the mode too, so a light app
        // does not open with a dark search box in it.
        gpui_component::Theme::change(
            if theme_pref.mode == "light" {
                gpui_component::ThemeMode::Light
            } else {
                gpui_component::ThemeMode::Dark
            },
            None,
            cx,
        );

        let mut this = Self {
            state: AppState::default(),
            api_id_input,
            api_hash_input,
            phone_input,
            code_input,
            password_input,
            search_input,
            vault_input,
            vault_focus_pending: false,
            theme_pref,
            upload_hook_done: false,
            menu_open: false,
            menu_view: MenuView::Account,
            encrypt_password_input,
            encrypt_confirm_input,
            encrypt_hint_input,
            rename_input,
            folder_input,
            rows_cache: RefCell::new(None),
            gallery_list: ListState::new(0, ListAlignment::Top, px(400.)),
            gallery_list_version: u64::MAX,
            viewport_w: 1285.,
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
            (
                this.encrypt_password_input.clone(),
                LoginAction::SubmitEncryptionSetup,
            ),
            (
                this.encrypt_confirm_input.clone(),
                LoginAction::SubmitEncryptionSetup,
            ),
            (
                this.encrypt_hint_input.clone(),
                LoginAction::SubmitEncryptionSetup,
            ),
            (this.rename_input.clone(), LoginAction::SubmitRename),
            (this.folder_input.clone(), LoginAction::SubmitFolder),
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

/// What the account menu is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuView {
    /// The account itself, and the things one can do to it.
    Account,
    /// The appearance panel, with the theme catalogue.
    Appearance,
}

// --- Small building blocks --------------------------------------------------

/// The account menu's trigger, at the end of the header.
fn profile_trigger(view: Entity<TDriveView>) -> impl IntoElement {
    div()
        .id("profile-trigger")
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .h(px(32.))
        .px(px(6.))
        .rounded(px(16.))
        .cursor_pointer()
        .hover(|el| el.bg(theme::overlay_1()))
        .child(
            div()
                .size(px(24.))
                .rounded(px(12.))
                .bg(theme::accent())
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.))
                .font_weight(FontWeight::BOLD)
                .text_color(theme::on_accent())
                .child("T"),
        )
        .child(glyph(IconName::ChevronDown, 14., theme::text_muted()))
        .on_click(move |_, _window, cx| {
            view.update(cx, |this, cx| {
                this.menu_open = !this.menu_open;
                this.menu_view = MenuView::Account;
                cx.notify();
            });
        })
}

/// A theme's own picture of the shell, the way the original's palette card
/// shows it: a sidebar, a top line and two rows.
fn theme_preview(theme_def: &theme::Theme) -> impl IntoElement {
    let [canvas, surface, accent, text] = theme_def.preview;
    div()
        .h(px(62.))
        .w_full()
        .flex()
        .flex_row()
        .gap(px(4.))
        .p(px(4.))
        .rounded(px(6.))
        .bg(rgb(canvas))
        .child(
            // The sidebar.
            div()
                .w(px(32.))
                .h_full()
                .rounded(px(4.))
                .bg(rgb(surface))
                .flex()
                .flex_col()
                .gap(px(4.))
                .p(px(4.))
                .child(div().h(px(4.)).w(px(10.)).rounded(px(2.)).bg(rgb(accent)))
                .child(div().h(px(3.)).w_full().rounded(px(1.)).bg(rgb(text)))
                .child(div().h(px(3.)).w(px(8.)).rounded(px(1.)).bg(rgb(text))),
        )
        .child(
            // The content.
            div()
                .flex_1()
                .h_full()
                .rounded(px(4.))
                .bg(rgb(surface))
                .flex()
                .flex_col()
                .gap(px(5.))
                .p(px(5.))
                .child(div().h(px(4.)).w(px(24.)).rounded(px(2.)).bg(rgb(text)))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(4.))
                        .child(div().size(px(7.)).rounded(px(2.)).bg(rgb(accent)))
                        .child(div().h(px(3.)).w(px(16.)).rounded(px(1.)).bg(rgb(text))),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(4.))
                        .child(div().size(px(7.)).rounded(px(2.)).bg(rgb(text)))
                        .child(div().h(px(3.)).w(px(10.)).rounded(px(1.)).bg(rgb(text))),
                ),
        )
}

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
    on_click: impl Fn(&mut Window, &mut App) + 'static,
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
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            on_click(window, cx);
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.viewport_w = f32::from(window.bounds().size.width);
        // A prompt that has just been opened takes the keyboard with it. The
        // focus is given here, while the frame is being drawn, because the
        // click that opened the prompt is still finishing and would take the
        // focus back for itself.
        if self.vault_focus_pending {
            self.vault_focus_pending = false;
            let input = self.vault_input.clone();
            cx.defer_in(window, move |_this, window, cx| {
                input.update(cx, |state, cx| state.focus(window, cx));
            });
        }
        let content = match self.state.login.step {
            LoginStep::LoggedIn => self.render_app(cx).into_any_element(),
            // Signed in but nothing saved on this device yet: the original asks
            // which channel is the drive before it shows any files.
            LoginStep::ChooseDrive => self.render_drive_picker(cx),
            _ => self.render_login(cx).into_any_element(),
        };
        let preview = render_preview(&self.state, {
            let view = cx.entity();
            move |cx: &mut App| {
                view.update(cx, |this, cx| {
                    this.state.preview = None;
                    cx.notify();
                });
            }
        });
        let rename_dialog = self.render_rename_dialog(cx);
        let move_dialog = self.render_move_dialog(cx);
        let folder_dialog = self.render_folder_dialog(cx);
        let vault_prompt = self.render_vault_prompt(cx);
        let encryption_setup = self.render_encryption_setup(cx);
        let upload_options = self.render_upload_options(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .relative()
            .bg(theme::canvas())
            .text_color(theme::text_soft())
            .child(content)
            .children(preview)
            .children(rename_dialog)
            .children(move_dialog)
            .children(folder_dialog)
            .children(vault_prompt)
            .children(encryption_setup)
            .children(upload_options)
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
                                            this.state.cancel_vault_prompt();
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

    /// The rename dialog, drawn over the shell while it is open.
    fn render_rename_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let target = self.state.pending_rename.as_ref()?;
        let is_folder = target.is_folder;
        let working = target.working;
        let name = self.rename_input.read(cx).value().to_string();
        let view = cx.entity();
        let submit_view = view.clone();
        let cancel_view = view.clone();
        let title = if is_folder {
            "Rename folder"
        } else {
            "Rename file"
        };
        let subtitle = if is_folder {
            "Choose a new folder name."
        } else {
            "Choose a new file name."
        };
        let label = if is_folder {
            "Folder name"
        } else {
            "File name"
        };
        Some(modal_scrim(
            modal_card(390.)
                .child(modal_title(title))
                .child(modal_subtitle(subtitle))
                .child(field(label, text_field(&self.rename_input)))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(10.))
                        .child(
                            Button::new("rename-confirm")
                                .primary()
                                .label("Rename")
                                .disabled(working || name.trim().is_empty())
                                .on_click(move |_, _window, cx| {
                                    commit_rename(submit_view.clone(), cx);
                                }),
                        )
                        .child(
                            Button::new("rename-cancel")
                                .ghost()
                                .label("Cancel")
                                .disabled(working)
                                .on_click(move |_, _window, cx| {
                                    let view = cancel_view.clone();
                                    view.update(cx, |this, cx| {
                                        this.state.close_rename();
                                        cx.notify();
                                    });
                                }),
                        ),
                ),
        ))
    }

    /// The destination picker, drawn over the shell while a move is being
    /// decided. It browses the drive's folders from the root, the way the
    /// original's picker does, and refuses the item's own folder and, for a
    /// folder, its own subtree.
    fn render_move_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let target = self.state.pending_move.as_ref()?;
        let working = target.working;
        let path = self.state.move_path.clone();
        let current = path.last()?;
        let current_id = current.id.clone();
        let current_name = current.name.clone();
        let blocked = self.state.move_blocked();
        let folders = self.state.folders_in(&current_id);
        let can_move = self.state.move_available() && !working;
        let at_source = current_id == self.state.move_source_parent();
        let title = if target.items.len() == 1 {
            format!("Move \"{}\"", target.items[0].name)
        } else {
            format!("Move {} items", target.items.len())
        };
        let view = cx.entity();

        // The trail down to the folder being browsed: the drive's name first,
        // then one crumb per folder walked into.
        let mut trail = div().flex().flex_row().items_center().gap(px(6.));
        for (index, crumb) in path.iter().enumerate() {
            let last = index + 1 == path.len();
            let crumb_view = view.clone();
            let crumb_index = index;
            let label = div()
                .text_size(px(theme::TYPE_SM))
                .text_color(if last {
                    theme::text()
                } else {
                    theme::text_muted()
                })
                .child(crumb.name.clone());
            trail = trail.child(
                div()
                    .id(SharedString::from(format!("move-crumb-{index}")))
                    .when(!last, |el| {
                        el.cursor_pointer()
                            .rounded(px(4.))
                            .px(px(3.))
                            .hover(|style| style.bg(theme::overlay_2()))
                            .on_click(move |_, _window, cx| {
                                let view = crumb_view.clone();
                                view.update(cx, |this, cx| {
                                    this.state.move_path.truncate(crumb_index + 1);
                                    cx.notify();
                                });
                            })
                    })
                    .child(label),
            );
            if !last {
                trail = trail.child(
                    div()
                        .text_size(px(theme::TYPE_SM))
                        .text_color(theme::text_subtle())
                        .child("/"),
                );
            }
        }

        let back_view = view.clone();
        let can_back = path.len() > 1;
        let back = div()
            .id("move-back")
            .w(px(26.))
            .h(px(26.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.))
            .opacity(if can_back { 1.0 } else { 0.35 })
            .when(can_back, |el| {
                el.cursor_pointer()
                    .hover(|style| style.bg(theme::overlay_2()))
                    .on_click(move |_, _window, cx| {
                        let view = back_view.clone();
                        view.update(cx, |this, cx| {
                            if this.state.move_path.len() > 1 {
                                this.state.move_path.pop();
                            }
                            cx.notify();
                        });
                    })
            })
            .child(glyph(
                IconName::ChevronLeft,
                14.,
                if can_back {
                    theme::text()
                } else {
                    theme::text_subtle()
                },
            ));

        let mut listing = div().flex().flex_col().gap(px(2.));
        if folders.is_empty() {
            listing = listing.child(
                div()
                    .py(px(14.))
                    .text_size(px(theme::TYPE_SM))
                    .text_color(theme::text_muted())
                    .child("No folders here."),
            );
        }
        for (id, name) in folders {
            let folder_blocked_or_here = blocked.contains(&id);
            let open_view = view.clone();
            let folder_id = id.clone();
            let entry = div()
                .id(SharedString::from(format!("move-to-{id}")))
                .h(px(34.))
                .px(px(8.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .rounded(px(6.))
                .text_size(px(theme::TYPE_SM))
                .text_color(if folder_blocked_or_here {
                    theme::text_subtle()
                } else {
                    theme::text_soft()
                })
                .when(!folder_blocked_or_here, |el| {
                    el.cursor_pointer()
                        .hover(|style| style.bg(theme::overlay_2()))
                        .on_click(move |_, _window, cx| {
                            let view = open_view.clone();
                            let id = folder_id.clone();
                            view.update(cx, |this, cx| {
                                let name = this
                                    .state
                                    .files
                                    .iter()
                                    .find(|file| file.object_id == id)
                                    .map(|file| file.name.clone())
                                    .unwrap_or_default();
                                this.state.move_path.push(Crumb { id, name });
                                cx.notify();
                            });
                        })
                })
                .child(glyph(
                    IconName::Folder,
                    14.,
                    if folder_blocked_or_here {
                        theme::text_subtle()
                    } else {
                        theme::text_muted()
                    },
                ))
                .child(div().min_w_0().truncate().child(name));
            listing = listing.child(entry);
        }

        let confirm_view = view.clone();
        let cancel_view = view.clone();
        Some(modal_scrim(
            modal_card(420.)
                .child(modal_title(title))
                .child(modal_subtitle("Select destination folder"))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .child(back)
                        .child(trail),
                )
                .child(
                    div()
                        .id("move-list")
                        .flex()
                        .flex_col()
                        .h(px(230.))
                        .overflow_y_scroll()
                        .border_1()
                        .border_color(theme::border_soft())
                        .rounded(px(8.))
                        .p(px(4.))
                        .child(listing),
                )
                .children(at_source.then(|| {
                    div()
                        .text_size(px(theme::TYPE_XS))
                        .text_color(theme::text_muted())
                        .child("The items are already in this folder.")
                }))
                .children(blocked.contains(&current_id).then(|| {
                    div()
                        .text_size(px(theme::TYPE_XS))
                        .text_color(theme::danger())
                        .child("You can't move a folder into itself.")
                }))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(10.))
                        .child(
                            Button::new("move-confirm")
                                .primary()
                                .label(format!("Move to \"{current_name}\""))
                                .disabled(!can_move)
                                .on_click(move |_, _window, cx| {
                                    commit_move(confirm_view.clone(), cx);
                                }),
                        )
                        .child(
                            Button::new("move-cancel")
                                .ghost()
                                .label("Cancel")
                                .disabled(working)
                                .on_click(move |_, _window, cx| {
                                    let view = cancel_view.clone();
                                    view.update(cx, |this, cx| {
                                        this.state.close_move();
                                        cx.notify();
                                    });
                                }),
                        ),
                ),
        ))
    }

    /// The "New folder" dialog.
    fn render_folder_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.state.pending_folder {
            return None;
        }
        let name = self.folder_input.read(cx).value().to_string();
        let view = cx.entity();
        let submit_view = view.clone();
        let cancel_view = view.clone();
        Some(modal_scrim(
            modal_card(390.)
                .child(modal_title("New folder"))
                .child(modal_subtitle("Create a folder in the current location."))
                .child(field("Folder name", text_field(&self.folder_input)))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(10.))
                        .child(
                            Button::new("folder-create")
                                .primary()
                                .label("Create folder")
                                .disabled(name.trim().is_empty())
                                .on_click(move |_, _window, cx| {
                                    commit_folder(submit_view.clone(), cx);
                                }),
                        )
                        .child(
                            Button::new("folder-cancel")
                                .ghost()
                                .label("Cancel")
                                .on_click(move |_, _window, cx| {
                                    let view = cancel_view.clone();
                                    view.update(cx, |this, cx| {
                                        this.state.pending_folder = false;
                                        cx.notify();
                                    });
                                }),
                        ),
                ),
        ))
    }

    /// The first-time encryption setup, drawn over whatever is beneath it.
    ///
    /// The password is the drive's own, so it is asked for twice and guarded
    /// with the original's warning; the hint travels with the config in the
    /// clear, which the form says out loud.
    fn render_encryption_setup(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let setup = self.state.encryption_setup.as_ref()?;
        let error = setup.error.clone();
        let working = setup.working;
        let view = cx.entity();
        let submit_view = view.clone();
        let cancel_view = view.clone();

        let mut confirm = Button::new("encrypt-confirm")
            .primary()
            .label("Set password");
        if !working {
            confirm = confirm.on_click(move |_, _window, cx| {
                submit_encryption_setup(submit_view.clone(), cx);
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
                        .w(px(430.))
                        .flex()
                        .flex_col()
                        .gap(px(10.))
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
                                .child("Set an encryption password"),
                        )
                        .child(
                            div()
                                .text_size(px(theme::TYPE_SM))
                                .text_color(theme::text_muted())
                                .child(
                                    "Use one password for every encrypted file in My Drive. \
                                     If you forget it, encrypted files cannot be recovered.",
                                ),
                        )
                        .child(text_field(&self.encrypt_password_input))
                        .child(text_field(&self.encrypt_confirm_input))
                        .child(text_field(&self.encrypt_hint_input))
                        .child(
                            div()
                                .text_size(px(theme::TYPE_XS))
                                .text_color(theme::text_subtle())
                                .child(
                                    "This hint is shown when TDrive asks for your encryption \
                                     password. It is not encrypted, so don't put the password \
                                     itself here.",
                                ),
                        )
                        .children(error.map(error_banner))
                        .child(
                            div().flex().flex_row().gap(px(10.)).child(confirm).child(
                                Button::new("encrypt-cancel")
                                    .ghost()
                                    .label("Cancel")
                                    .on_click(move |_, _window, cx| {
                                        cancel_view.update(cx, |this, cx| {
                                            this.state.close_encryption_setup();
                                            // Leaving the form is how a
                                            // waiting encrypted upload is
                                            // called off.
                                            if this.state.pending_upload.is_some() {
                                                this.state.pending_upload = None;
                                                this.state.status = "Upload cancelled".to_string();
                                            }
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

    /// "How would you like to upload?", the choice the original puts in front
    /// of every upload to My Drive.
    ///
    /// The batch starts on plain every time: encryption is an explicit choice
    /// per upload, never something remembered from the last one.
    fn render_upload_options(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let options = self.state.upload_options.as_ref()?;
        let summary = if options.count == 1 {
            "Upload 1 file".to_string()
        } else {
            format!("Upload {} files", options.count)
        };
        let view = cx.entity();
        let plain_view = view.clone();
        let encrypt_view = view.clone();
        let cancel_view = view.clone();
        let confirm_view = view.clone();

        let choice =
            |id: &'static str, selected: bool, title: &'static str, body: &'static str| {
                div()
                    .id(id)
                    .role(gpui::Role::RadioButton)
                    .aria_label(title)
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(10.))
                    .p(px(12.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .border_1()
                    .border_color(if selected {
                        theme::accent()
                    } else {
                        theme::border_soft()
                    })
                    .when(selected, |el| el.bg(theme::overlay_accent_1()))
                    .hover(|style| style.bg(theme::panel()))
                    .child(
                        div()
                            .w(px(16.))
                            .h(px(16.))
                            .flex_shrink_0()
                            .rounded_full()
                            .border_1()
                            .border_color(if selected {
                                theme::accent()
                            } else {
                                theme::text_muted()
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .children(selected.then(|| {
                                div().w(px(8.)).h(px(8.)).rounded_full().bg(theme::accent())
                            })),
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
                                    .text_size(px(theme::TYPE_BASE))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text())
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::TYPE_XS))
                                    .text_color(theme::text_muted())
                                    .child(body),
                            ),
                    )
            };

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
                        .w(px(470.))
                        .flex()
                        .flex_col()
                        .gap(px(10.))
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
                                .child("How would you like to upload?"),
                        )
                        .child(
                            div()
                                .text_size(px(theme::TYPE_SM))
                                .text_color(theme::text_muted())
                                .child(summary),
                        )
                        .child(
                            choice(
                                "upload-mode-plain",
                                !options.encrypt,
                                "Upload",
                                "Files are stored on Telegram as-is.",
                            )
                            .on_click({
                                let view = plain_view;
                                move |_, _window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.state.set_upload_encrypt(false);
                                        cx.notify();
                                    });
                                }
                            }),
                        )
                        .child(
                            choice(
                                "upload-mode-encrypt",
                                options.encrypt,
                                "Encrypt before upload",
                                "TDrive encrypts file contents before sending them to Telegram. \
                                 Names and folders stay visible.",
                            )
                            .on_click({
                                let view = encrypt_view;
                                move |_, _window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.state.set_upload_encrypt(true);
                                        cx.notify();
                                    });
                                }
                            }),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(10.))
                                .child(
                                    Button::new("upload-options-confirm")
                                        .primary()
                                        .label("Continue")
                                        .on_click({
                                            let view = confirm_view;
                                            move |_, _window, cx| {
                                                submit_upload_options(view.clone(), cx);
                                            }
                                        }),
                                )
                                .child(
                                    Button::new("upload-options-cancel")
                                        .ghost()
                                        .label("Cancel")
                                        .on_click({
                                            let view = cancel_view;
                                            move |_, _window, cx| {
                                                view.update(cx, |this, cx| {
                                                    this.state.cancel_upload();
                                                    this.state.status =
                                                        "Upload cancelled".to_string();
                                                    cx.notify();
                                                });
                                            }
                                        }),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    // --- Application shell --------------------------------------------------
    fn render_app(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
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

        let gallery_view = view.clone();
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
                        "Gallery".to_string(),
                        view_mode == ViewMode::Gallery,
                        move |cx| {
                            gallery_view.update(cx, |this, cx| {
                                this.state.set_view(ViewMode::Gallery);
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

    fn render_main(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        let trash_mode = self.state.trash_mode();
        let gallery_mode = self.state.gallery_mode();
        // In the gallery the file table and the folder trail step aside for the
        // grid, exactly as the original's photos mode hides them.
        let gallery_only = gallery_mode && !trash_mode;
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
        let new_folder_view = view.clone();

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
                                    // A reload is the way back to the truth the
                                    // drive itself holds: the messages that
                                    // arrived since the last read are read
                                    // again, which is what the original's
                                    // reload does and what keeps a reload
                                    // quick. It keeps the folder on screen,
                                    // as the original's does.
                                    spawn_load_drive(reload_view.clone(), index, true, cx);
                                }
                            })
                    }))
                    .children((!trash_mode && !gallery_only).then(|| {
                        let folder_view = new_folder_view.clone();
                        Button::new("new-folder")
                            .ghost()
                            .icon(IconName::FolderPlus)
                            .tooltip("New folder")
                            .on_click(move |_, window, cx| {
                                let view = folder_view.clone();
                                view.update(cx, |this, cx| {
                                    this.state.pending_folder = true;
                                    cx.notify();
                                });
                                let input = view.read(cx).folder_input.clone();
                                input.update(cx, |state, cx| {
                                    state.set_value("", window, cx);
                                    state.focus(window, cx);
                                });
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
                    }))
                    // The account menu, at the end of the header's actions, as
                    // it is in the original's shell.
                    .child(profile_trigger(view.clone())),
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
        // weight and scale as the Gallery destination it sits beside.
        let trash_heading = div()
            .flex_shrink_0()
            .text_size(px(theme::TYPE_LG))
            .font_weight(FontWeight::BOLD)
            .text_color(theme::text())
            .child("Trash");

        // The gallery replaces the folder trail with its own title, because a
        // drive-wide grid is not inside a folder at all.
        let gallery_heading = div()
            .flex_shrink_0()
            .text_size(px(theme::TYPE_LG))
            .font_weight(FontWeight::BOLD)
            .text_color(theme::text())
            .child("Gallery");

        // A drive whose media is encrypted opens the gallery as a wall of
        // locks, and there is nothing in the grid itself to click to change
        // that. Offer the unlock next to the title, where the pictures are.
        let locked_gallery = gallery_only
            && self
                .state
                .vault
                .as_ref()
                .is_some_and(|vault| vault.key.is_none())
            && self.state.files.iter().any(|file| file.encrypted);
        let unlock_view = view.clone();
        let gallery_unlock = locked_gallery.then(|| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .text_size(px(theme::TYPE_XS))
                        .text_color(theme::text_muted())
                        .child("Encrypted files are locked"),
                )
                .child(
                    Button::new("gallery-unlock")
                        .primary()
                        .label("Unlock")
                        .on_click(move |_, _window, cx| {
                            let view = unlock_view.clone();
                            view.update(cx, |this, cx| {
                                this.state.request_vault_for(VaultResume::Nothing, None);
                                this.vault_focus_pending = true;
                                cx.notify();
                            });
                        }),
                )
        });

        // The gallery says what an encrypted picture costs, because it costs
        // the whole file: the bytes on Telegram are ciphertext and carry no
        // rendition of their own, so a picture can only be made by fetching
        // the file itself.
        let gallery_note =
            (gallery_only && !locked_gallery && self.state.files.iter().any(|file| file.encrypted))
                .then(|| {
                    div()
                        .text_size(px(theme::TYPE_XS))
                        .text_color(theme::text_muted())
                        .child("Encrypted pictures cost their whole file to preview")
                });

        // The pictures that have no derivative yet are the ones the gallery
        // has to fetch whole. Making them is a job of its own, so it is
        // offered here, with the count of what it would do.
        let (without_derivative, derivative_bytes) = self
            .state
            .files
            .iter()
            .filter(|file| {
                file.kind == FileKind::Image
                    && !file.renditions.iter().any(|entry| entry.is_thumbnail())
            })
            .fold((0usize, 0i64), |(count, bytes), file| {
                (count + 1, bytes + file.size)
            });
        let job_running = self
            .state
            .derivative_job
            .as_ref()
            .is_some_and(|job| job.running);
        let derivative_button = (gallery_only && (without_derivative > 0 || job_running)).then(|| {
            let view = view.clone();
            let button: AnyElement = if job_running {
                let view = view.clone();
                div()
                    .id("derivative-stop")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .on_click(move |_, _window, cx| {
                        view.update(cx, |this, cx| {
                            if let Some(job) = this.state.derivative_job.as_mut() {
                                job.cancelled = true;
                            }
                            this.state.status = "Stopping after this picture".to_string();
                            cx.notify();
                        });
                    })
                    .child(
                        div()
                            .text_size(px(theme::TYPE_XS))
                            .text_color(theme::text_muted())
                            .child("Stop"),
                    )
                    .into_any_element()
            } else {
                Button::new("derivative-job")
                    .ghost()
                    .label(format!("Make previews ({without_derivative})"))
                    .tooltip(format!(
                        "Publish a small picture beside each picture, so the gallery does not have to fetch the whole file. This fetches the {} pictures once ({}) and publishes a small picture for each.",
                        without_derivative,
                        human_size(derivative_bytes)
                    ))
                    .on_click(move |_, _window, cx| {
                        spawn_derivative_job(view.clone(), cx);
                    })
                    .into_any_element()
            };
            button
        });

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
            // There is no folder to be at in the trash or the gallery, so there
            // is no trail to show and nowhere for Back to go: leaving is the
            // sidebar's job.
            .children((!trash_mode && !gallery_only).then_some(back))
            .children((!trash_mode && !gallery_only).then_some(trail))
            .children(trash_mode.then_some(trash_heading))
            .children(gallery_only.then_some(gallery_heading))
            .child(div().flex_1())
            .children(gallery_note)
            .children(derivative_button)
            .children(gallery_unlock)
            // Destructive and irreversible, so it only appears when there is
            // something to empty and only arms on the first click.
            .children(
                (trash_mode && trash_count > 0)
                    .then(|| empty_trash_button(view.clone(), empty_armed)),
            );
        let sort = self.state.sort;
        let date_label = if trash_mode {
            "Time left"
        } else if self.state.searching() {
            // A search spans every folder, so the column reports where each
            // result lives; the sort control stays the date's, as the
            // original's column-mode store leaves it.
            "Location"
        } else {
            "Date"
        };
        // While rows are picked, the header is covered by a bar that names
        // how many there are and offers what can be done with them, which is
        // where the original puts its selection bar.
        let picked = self.state.selected_objects.len();
        let table_header = div()
            .relative()
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
            )
            .children((picked > 0).then(|| selection_bar(view.clone(), picked)));

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
            .relative()
            .bg(theme::canvas())
            .child(header)
            .child(breadcrumb)
            .children(banner)
            .children((!gallery_only).then_some(table_header))
            .child(if gallery_only {
                self.render_gallery(loading, cx)
            } else {
                self.render_file_list(loading, cx)
            })
            .children(self.render_profile_menu(cx))
            .into_any_element()
    }

    /// Put a theme on screen and remember it.
    ///
    /// The theme is remembered beside the drive list, in the same file the
    /// original keeps its own settings, so the next launch opens in the theme
    /// the user left it in.
    fn set_theme(&mut self, id: &str, cx: &mut Context<Self>) {
        if !theme::set_theme(id) {
            return;
        }
        if theme::theme_by_id(id).is_some_and(|entry| entry.appearance == theme::Appearance::Light)
        {
            self.theme_pref.mode = "light".to_string();
            self.theme_pref.light = id.to_string();
        } else {
            self.theme_pref.mode = "dark".to_string();
            self.theme_pref.dark = id.to_string();
        }
        self.save_theme_pref();
        self.apply_component_theme(cx);
        cx.notify();
    }

    /// Switch between the light and the dark mode, taking the theme each mode
    /// was last left in.
    fn set_theme_mode(&mut self, mode: &str, cx: &mut Context<Self>) {
        self.theme_pref.mode = mode.to_string();
        let id = self.theme_pref.active().to_string();
        theme::set_theme(&id);
        self.save_theme_pref();
        self.apply_component_theme(cx);
        cx.notify();
    }

    /// Put the borrowed controls in the mode the app is in.
    ///
    /// The search box, the buttons and the menus come from the component
    /// library, which has a mode of its own; a light app with dark controls in
    /// it is not the original's look.
    fn apply_component_theme(&self, cx: &mut Context<Self>) {
        let mode = if self.theme_pref.mode == "light" {
            gpui_component::ThemeMode::Light
        } else {
            gpui_component::ThemeMode::Dark
        };
        gpui_component::Theme::change(mode, None, cx);
    }

    /// Write the theme preference down beside the drive list.
    fn save_theme_pref(&self) {
        let mut config = DriveConfig::load().unwrap_or_default();
        config.theme = self.theme_pref.clone();
        let _ = config.save();
    }

    /// The account menu, drawn over the shell while it is open.
    ///
    /// The original's menu holds the account, the appearance, the update check
    /// and the sign-out. This one holds the account and the appearance, which
    /// are the two the port can answer.
    fn render_profile_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.menu_open {
            return None;
        }
        let view = cx.entity();
        Some(
            div()
                .absolute()
                .top(px(theme::HEADER_H - 6.))
                .right(px(20.))
                .w(px(390.))
                .max_h(px(680.))
                .rounded(px(12.))
                .bg(theme::glass_bg())
                .border_1()
                .border_color(theme::glass_border())
                .child(match self.menu_view {
                    MenuView::Account => self.render_account_menu(view.clone()),
                    MenuView::Appearance => self.render_appearance_panel(view.clone()),
                })
                .into_any_element(),
        )
    }

    /// The account menu's own view: who is signed in, and the way to change
    /// how the app looks.
    fn render_account_menu(&self, view: Entity<TDriveView>) -> AnyElement {
        let appearance_view = view.clone();
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.))
                    .px(px(16.))
                    .py(px(14.))
                    .child(
                        div()
                            .size(px(36.))
                            .rounded(px(18.))
                            .bg(theme::accent())
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(15.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme::on_accent())
                            .child("T"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(theme::TYPE_BASE))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text())
                                    .child("Telegram account"),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::TYPE_XS))
                                    .text_color(theme::text_muted())
                                    .child("Signed in"),
                            ),
                    ),
            )
            .child(div().h(px(1.)).bg(theme::border_soft()))
            .child(
                div()
                    .id("profile-menu-appearance")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.))
                    .px(px(16.))
                    .py(px(12.))
                    .cursor_pointer()
                    .hover(|el| el.bg(theme::overlay_1()))
                    .child(glyph(IconName::Palette, 18., theme::text_muted()))
                    .child(
                        div()
                            .text_size(px(theme::TYPE_BASE))
                            .text_color(theme::text())
                            .child("Appearance"),
                    )
                    .on_click(move |_, _window, cx| {
                        appearance_view.update(cx, |this, cx| {
                            this.menu_view = MenuView::Appearance;
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    }

    /// The appearance panel: the light and dark modes, and the catalogue of
    /// palettes for the mode on screen.
    fn render_appearance_panel(&self, view: Entity<TDriveView>) -> AnyElement {
        let mode = if self.theme_pref.mode == "light" {
            theme::Appearance::Light
        } else {
            theme::Appearance::Dark
        };
        let active = self.theme_pref.active().to_string();
        let back_view = view.clone();

        let mut modes = div().flex().flex_row().gap(px(7.)).px(px(8.));
        for (id, label, icon) in [
            ("light", "Light", IconName::Sun),
            ("dark", "Dark", IconName::Moon),
        ] {
            let selected = self.theme_pref.mode == id;
            let card_view = view.clone();
            modes = modes.child(
                div()
                    .id(SharedString::from(format!("appearance-mode-{id}")))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(6.))
                    .h(px(68.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(if selected {
                        theme::overlay_accent_3()
                    } else {
                        theme::border_soft()
                    })
                    .bg(if selected {
                        theme::overlay_accent_1()
                    } else {
                        theme::overlay_white_1()
                    })
                    .cursor_pointer()
                    .hover(|el| el.bg(theme::overlay_2()))
                    .child(
                        div()
                            .size(px(26.))
                            .rounded(px(6.))
                            .bg(if selected {
                                theme::overlay_accent_2()
                            } else {
                                theme::overlay_white_1()
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(glyph(icon, 15., theme::text_muted())),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if selected {
                                theme::accent()
                            } else {
                                theme::text_muted()
                            })
                            .child(label),
                    )
                    .on_click(move |_, _window, cx| {
                        card_view.update(cx, |this, cx| {
                            this.set_theme_mode(id, cx);
                        });
                    }),
            );
        }

        let mut grid = div().flex().flex_row().flex_wrap().gap(px(8.)).px(px(8.));
        for entry in theme::THEMES {
            if entry.appearance != mode {
                continue;
            }
            let selected = entry.id == active;
            let card_view = view.clone();
            let id = entry.id;
            grid = grid.child(
                div()
                    .id(SharedString::from(format!("appearance-theme-{id}")))
                    .w(px(178.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .p(px(7.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(if selected {
                        theme::accent()
                    } else {
                        theme::border_soft()
                    })
                    .bg(if selected {
                        theme::overlay_accent_1()
                    } else {
                        theme::overlay_white_1()
                    })
                    .cursor_pointer()
                    .hover(|el| el.bg(theme::overlay_2()))
                    .child(theme_preview(entry))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .px(px(2.))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme::text())
                                    .child(entry.name),
                            )
                            .child(
                                div()
                                    .size(px(14.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(selected, |el| {
                                        el.child(glyph(IconName::Check, 13., theme::accent()))
                                    }),
                            ),
                    )
                    .on_click(move |_, _window, cx| {
                        card_view.update(cx, |this, cx| {
                            this.set_theme(id, cx);
                        });
                    }),
            );
        }

        let name = theme::theme_by_id(&active)
            .map(|entry| entry.name.to_string())
            .unwrap_or_default();

        div()
            .id("appearance-scroll")
            .flex()
            .flex_col()
            .max_h(px(680.))
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px(px(16.))
                    .py(px(12.))
                    .border_b_1()
                    .border_color(theme::border_soft())
                    .child(
                        div()
                            .text_size(px(17.))
                            .font_weight(FontWeight::EXTRA_BOLD)
                            .text_color(theme::text())
                            .child("Appearance"),
                    )
                    .child(
                        div()
                            .id("appearance-back")
                            .cursor_pointer()
                            .text_size(px(theme::TYPE_SM))
                            .text_color(theme::text_muted())
                            .child("Back")
                            .on_click(move |_, _window, cx| {
                                back_view.update(cx, |this, cx| {
                                    this.menu_view = MenuView::Account;
                                    cx.notify();
                                });
                            }),
                    ),
            )
            .child(modes)
            .child(
                div()
                    .px(px(16.))
                    .pt(px(14.))
                    .pb(px(8.))
                    .text_size(px(11.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme::text_muted())
                    .child(if mode == theme::Appearance::Light {
                        "Light palettes"
                    } else {
                        "Dark palettes"
                    }),
            )
            .child(grid)
            .child(
                div()
                    .px(px(16.))
                    .py(px(10.))
                    .text_size(px(theme::TYPE_XS))
                    .text_color(theme::text_muted())
                    .child(format!("{name} is active.")),
            )
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

    /// Where one row of a search lives, in place of its date. `None` outside a
    /// search, where the column reports the date the row was added instead.
    fn search_label(&self, index: usize) -> Option<String> {
        let cache = self.rows_cache.borrow();
        cache
            .as_ref()
            .and_then(|cache| cache.labels.get(&index).cloned())
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
        let labels = Rc::new(if self.state.searching() {
            // One parent index for the whole result set, rather than a walk of
            // the drive per row.
            let folders = self.state.folder_index();
            rows.iter()
                .map(|&file| {
                    let row = &self.state.files[file];
                    let from = if row.is_folder() {
                        row.object_id.as_str()
                    } else {
                        row.parent.as_str()
                    };
                    (file, AppState::folder_path(&folders, from))
                })
                .collect::<BTreeMap<usize, String>>()
        } else {
            BTreeMap::new()
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
            labels,
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
            // A large drive is read in one go the first time, so the read says
            // where it is rather than leaving the reader to guess.
            let progress = self.state.load_progress.unwrap_or_default();
            return div()
                .id("file-list")
                .flex_1()
                .min_h_0()
                .px(px(24.))
                .py(px(16.))
                .flex()
                .flex_col()
                .gap(px(14.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(theme::TYPE_MD))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::text())
                                .child("Loading files..."),
                        )
                        .child(progress_bar(self.state.load_sweep))
                        .child(
                            div()
                                .text_size(px(theme::TYPE_SM))
                                .text_color(theme::text_muted())
                                .child(match progress.waiting {
                                    // Telegram has asked the read to wait. It
                                    // says so rather than looking stuck.
                                    Some(seconds) => format!(
                                        "Telegram asked for a {seconds}-second pause; the read continues when it lets us. {} messages read so far.",
                                        thousands(progress.scanned),
                                    ),
                                    None if progress.scanned == 0 => {
                                        "Reading the drive's operations from Telegram. Large drives can take a while the first time.".to_string()
                                    }
                                    None => format!(
                                        "{} messages read · {} operations · the rest of the app is yours to use.",
                                        thousands(progress.scanned),
                                        thousands(progress.operations),
                                    )
                                }),
                        ),
                )
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
                "Anything you delete waits here until its time runs out, so you can put it back."
                    .to_string(),
            )
        } else if self.state.active_drive.is_none() {
            (
                IconName::HardDrive,
                "No drive selected",
                "Pick a drive on the left to see its files.".to_string(),
            )
        } else if self.state.gallery_mode() {
            (
                IconName::Images,
                "No photos or videos here",
                "This drive has no images or videos.".to_string(),
            )
        } else if self.state.searching() {
            (
                IconName::SearchX,
                "No matches",
                // The original names the query it found nothing for.
                format!("No results for \"{}\"", self.state.query.trim()),
            )
        } else {
            (
                IconName::Inbox,
                "This drive is empty",
                "Upload a file to put it in this drive.".to_string(),
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

    /// The photo gallery: a grid of thumbnails for the images in the active
    /// drive or folder. Each tile fetches its own thumbnail on demand, so a
    /// library is not downloaded in one go.
    fn render_gallery(&mut self, loading: bool, cx: &mut Context<Self>) -> AnyElement {
        if loading {
            return div()
                .id("gallery")
                .flex_1()
                .min_h_0()
                .px(px(24.))
                .py(px(16.))
                .flex()
                .flex_col()
                .gap(px(14.))
                .child(
                    div()
                        .text_size(px(theme::TYPE_MD))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child("Loading gallery..."),
                )
                .children((0..6).map(skeleton_row))
                .into_any_element();
        }

        let rows = self.ordered_rows();
        let indices: Vec<usize> = rows.iter().copied().collect();
        let view = cx.entity();

        if indices.is_empty() {
            return self.empty_state().into_any_element();
        }

        // Group media by their parent folder.
        let mut folder_names: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for f in &self.state.files {
            if f.is_folder() {
                folder_names.insert(f.object_id.clone(), f.name.clone());
            }
        }
        let root_name = self
            .state
            .active_dialog()
            .map(|d| d.title.clone())
            .unwrap_or_else(|| "My Drive".to_string());

        let mut groups: std::collections::BTreeMap<String, Vec<usize>> =
            std::collections::BTreeMap::new();
        for &index in &indices {
            let parent = self.state.files[index].parent.clone();
            groups.entry(parent).or_default().push(index);
        }

        let drive_id = self.state.active_dialog().map(|d| d.id).unwrap_or(0);

        // The original's grid is `repeat(auto-fill, minmax(172px, 1fr))` with a
        // 5px gap. Deriving the column count from the window width keeps the
        // tiles square and every row full, whatever the window is doing.
        const GAP: f32 = 5.;
        const MIN_CELL: f32 = 172.;
        const SIDEBAR_W: f32 = 240.;
        const VIEW_PAD: f32 = 48.;
        let content_w = (self.viewport_w - SIDEBAR_W - VIEW_PAD).max(MIN_CELL);
        let cols = (((content_w + GAP) / (MIN_CELL + GAP)).floor() as usize).max(1);
        let cell = ((content_w - GAP * (cols as f32 - 1.)) / cols as f32)
            .floor()
            .max(96.);

        // Virtualized: one row per (header or tile-row). Only visible rows
        // render, which is what keeps a large library scrollable.
        enum GalleryRow {
            Header(String, usize),
            Tiles(Vec<usize>),
        }
        let mut gallery_rows: Vec<GalleryRow> = Vec::new();
        for (parent, group_indices) in &groups {
            let header_name = if parent.is_empty() {
                root_name.clone()
            } else {
                folder_names
                    .get(parent)
                    .cloned()
                    .unwrap_or_else(|| parent.clone())
            };
            gallery_rows.push(GalleryRow::Header(header_name, group_indices.len()));
            for chunk in group_indices.chunks(cols) {
                gallery_rows.push(GalleryRow::Tiles(chunk.to_vec()));
            }
        }
        let row_count = gallery_rows.len();
        // Headers and tile rows differ in height, so the gallery needs the
        // variable-height list rather than `uniform_list`. Its state only has
        // to be rebuilt when the rows themselves change.
        if self.gallery_list_version != self.state.rows_version {
            self.gallery_list.reset(row_count);
            self.gallery_list_version = self.state.rows_version;
        }
        let list_state = self.gallery_list.clone();
        let rows = std::rc::Rc::new(gallery_rows);

        list(list_state, move |ix, _window, cx| {
            let this = view.read(cx);
            match &rows[ix] {
                GalleryRow::Header(name, count) => div()
                    .w_full()
                    .px(px(24.))
                    .pt(px(16.))
                    .pb(px(12.))
                    .text_size(px(theme::TYPE_BASE))
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme::text())
                    .child(format!("{name}  ({count} items)"))
                    .into_any_element(),
                GalleryRow::Tiles(indices) => {
                    let mut row = div().flex().flex_row().gap(px(GAP)).px(px(24.)).pb(px(GAP));
                    for &index in indices {
                        let file = this.state.files[index].clone();
                        let message_id = file.message_id;
                        let is_image = file.kind == FileKind::Image;
                        let is_video = file.kind == FileKind::Video;
                        let encrypted = file.encrypted;
                        let key = this.state.vault.as_ref().and_then(|v| v.key);
                        let thumb = thumb_path(drive_id, message_id);
                        let exists = thumb.exists();
                        let downloading = this.state.gallery_downloads.contains(&message_id);

                        let tile_view = view.clone();
                        // A locked encrypted item cannot be decoded, so it
                        // is never fetched: the bytes that would land on
                        // disk are ciphertext, and caching them would stop
                        // a later unlock from ever showing the picture.
                        let locked = encrypted && key.is_none();
                        // A plain file has a rendition of its own on
                        // Telegram, which costs a few kilobytes, so it is
                        // fetched as it scrolls past. An encrypted file has
                        // none: its bytes are ciphertext, so a picture of
                        // it costs the whole file. The gallery says what
                        // that costs and waits to be asked, rather than
                        // spending the reader's connection on every picture
                        // that scrolls by.
                        let cheap = !encrypted;
                        // A plain file is fetched as it scrolls past, because
                        // its rendition costs a few kilobytes. An encrypted
                        // one has a derivative published beside it in the
                        // drive, which costs a few kilobytes more; a file
                        // without one is worth the whole body, and not
                        // without asking.
                        let derivative = file.renditions.iter().find(|entry| entry.is_thumbnail());
                        let can_fetch = (is_image || is_video) && cheap;
                        let can_fetch_derivative =
                            (is_image || is_video) && !cheap && !locked && derivative.is_some();
                        // A picture that has been made is shown whether it
                        // was fetched for a few kilobytes or asked for.
                        let shown = exists && !locked;
                        // A picture of an encrypted file is worth asking
                        // for, but not worth a file too large to be one.
                        let askable = (is_image || is_video)
                            && !cheap
                            && !locked
                            && !exists
                            && derivative.is_none()
                            && file.size <= GALLERY_PREVIEW_LIMIT;
                        let too_big = (is_image || is_video)
                            && !cheap
                            && !locked
                            && !exists
                            && derivative.is_none()
                            && file.size > GALLERY_PREVIEW_LIMIT;
                        let tile = div()
                            .w(px(cell))
                            .h(px(cell))
                            .flex_shrink_0()
                            .relative()
                            .rounded(px(10.))
                            .overflow_hidden()
                            .bg(theme::surface_1())
                            .border_1()
                            .border_color(theme::border_soft())
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(shown, |el| {
                                el.child(
                                    img(thumb.clone()).size_full().object_fit(ObjectFit::Cover),
                                )
                            })
                            // The placeholder gives way to the button that
                            // asks for the picture, so the two do not sit on
                            // top of each other.
                            .when(!shown && !askable && !too_big, |el| {
                                el.child(glyph(
                                    if locked {
                                        IconName::Lock
                                    } else if is_video {
                                        IconName::Video
                                    } else {
                                        IconName::Image
                                    },
                                    (cell * 0.22).clamp(24., 44.),
                                    theme::text_muted(),
                                ))
                            })
                            // An encrypted picture is fetched only when the
                            // reader asks for it, and the button says what it
                            // will cost.
                            .when(askable, |el| {
                                el.child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .left_0()
                                        .size_full()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            div()
                                                .px(px(10.))
                                                .py(px(6.))
                                                .rounded(px(8.))
                                                .bg(theme::surface_2())
                                                .border_1()
                                                .border_color(theme::border_soft())
                                                .child(
                                                    Button::new(format!("load-preview-{index}"))
                                                        .ghost()
                                                        .label(format!(
                                                            "Load preview · {}",
                                                            human_size(file.size)
                                                        ))
                                                        .on_click({
                                                            let load_view = tile_view.clone();
                                                            move |_, _window, cx| {
                                                                // The button is
                                                                // the picture's
                                                                // own action, so
                                                                // it does not
                                                                // also open the
                                                                // file.
                                                                cx.stop_propagation();
                                                                spawn_gallery_preview(
                                                                    load_view.clone(),
                                                                    index,
                                                                    cx,
                                                                );
                                                            }
                                                        }),
                                                ),
                                        ),
                                )
                            })
                            .when(too_big, |el| {
                                el.child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .left_0()
                                        .size_full()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            div()
                                                .px(px(8.))
                                                .text_size(px(theme::TYPE_XS))
                                                .text_color(theme::text_muted())
                                                .child("Too large to preview"),
                                        ),
                                )
                            })
                            // A video says so with a play badge, and an
                            // encrypted item carries the same lock the file
                            // list draws, both floating over the artwork.
                            .when(is_video && !locked, |el| {
                                el.child(
                                    div()
                                        .absolute()
                                        .right(px(8.))
                                        .bottom(px(8.))
                                        .w(px(28.))
                                        .h(px(28.))
                                        .rounded(px(999.))
                                        .bg(theme::overlay_2())
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(glyph(IconName::Play, 14., theme::text())),
                                )
                            })
                            .when(encrypted && !locked, |el| {
                                el.child(
                                    div()
                                        .absolute()
                                        .right(px(8.))
                                        .top(px(8.))
                                        .w(px(22.))
                                        .h(px(22.))
                                        .rounded(px(6.))
                                        .bg(theme::overlay_2())
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(glyph(IconName::Lock, 13., theme::text())),
                                )
                            })
                            // A tile opens its file, the way the original's
                            // gallery cell opens it in the viewer.
                            .id(SharedString::from(format!("tile-{index}")))
                            .cursor_pointer()
                            .on_click({
                                let tile_view = tile_view.clone();
                                move |_, _window, cx| {
                                    spawn_preview(tile_view.clone(), index, cx);
                                }
                            });

                        if (can_fetch || can_fetch_derivative) && !exists && !downloading {
                            let thumb = thumb.clone();
                            let tile_view = tile_view.clone();
                            let encrypted = encrypted;
                            let key = key;
                            let message_id = message_id;
                            let client = this.state.tele.clone();
                            let peer_ref = this.state.active_dialog().map(|d| d.peer_ref);
                            let derivative = derivative.cloned();
                            cx.spawn(async move |cx| {
                                tile_view.update(cx, |s, cx| {
                                    s.state.gallery_downloads.insert(message_id);
                                    cx.notify();
                                });
                                let result = cx
                                    .background_spawn(async move {
                                        if let Some(parent) = thumb.parent() {
                                            let _ = std::fs::create_dir_all(parent);
                                        }
                                        let (Some(client), Some(peer_ref)) = (client, peer_ref)
                                        else {
                                            return;
                                        };
                                        let messages = vec![message_id];
                                        // An encrypted body is ciphertext on
                                        // the wire: Telegram has no picture of
                                        // it, so the only way to a preview is to
                                        // pull the whole thing and decrypt it.
                                        if encrypted {
                                            // The drive holds a derivative
                                            // published beside the file, and
                                            // that is what the tile is drawn
                                            // from: a few kilobytes rather
                                            // than the whole picture.
                                            let (Some(derivative), Some(key)) = (derivative, key)
                                            else {
                                                return;
                                            };
                                            let bytes = match tele::download_rendition(
                                                &client,
                                                peer_ref,
                                                derivative.message_id,
                                            ) {
                                                Ok(bytes) => bytes,
                                                Err(_) => return,
                                            };
                                            let picture = match rendition::decrypt(
                                                &bytes,
                                                &key,
                                                &derivative.descriptor,
                                            ) {
                                                Ok(picture) => picture,
                                                Err(_) => return,
                                            };
                                            // The picture is written beside
                                            // its place and renamed into
                                            // it, so a reader never sees
                                            // half of one.
                                            let staged = thumb.with_extension("tdrive-part");
                                            if std::fs::write(&staged, picture).is_ok() {
                                                let _ = std::fs::rename(&staged, &thumb);
                                            }
                                        } else {
                                            // Everything else is painted from
                                            // Telegram's own rendition, which is
                                            // a fraction of the size.
                                            let thumbnailed = matches!(
                                                tele::download_thumbnail(
                                                    &client, peer_ref, message_id, &thumb,
                                                ),
                                                Ok(true)
                                            );
                                            // A plain image with no rendition is
                                            // still worth the whole body; a
                                            // whole video is not.
                                            if !thumbnailed && is_image {
                                                let _ = tele::download_file(
                                                    &client, peer_ref, &messages, &thumb, None,
                                                );
                                            }
                                        }
                                    })
                                    .await;
                                let _ = result;
                                cx.update(|cx| {
                                    tile_view.update(cx, |this, cx| {
                                        this.state.gallery_downloads.remove(&message_id);
                                        cx.notify();
                                    });
                                });
                            })
                            .detach();
                        }

                        row = row.child(tile);
                    }
                    row.into_any_element()
                }
            }
        })
        .flex_1()
        .min_h_0()
        .into_any_element()
    }
    fn render_file_row(&self, index: usize, view: &Entity<TDriveView>) -> AnyElement {
        let view = view.clone();
        let file = self.state.files[index].clone();
        let selected = self.state.is_selected(index);
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

        let date = match self.search_label(index) {
            Some(label) => label,
            None => short_date(file.date),
        };
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

        let rename_view = view.clone();
        let move_view = view.clone();
        // Folders carry the same rename and move actions as files and one fewer:
        // a folder has no body to download on its own.
        let mut buttons: Vec<AnyElement> = Vec::new();
        if confirming {
            buttons.push(row_action(
                SharedString::from(format!("confirm-{index}")),
                group.clone(),
                false,
                IconName::Check,
                false,
                "Confirm delete",
                move |_window, cx| spawn_delete(confirm_view.clone(), index, cx),
            ));
            buttons.push(row_action(
                SharedString::from(format!("cancel-{index}")),
                group.clone(),
                false,
                IconName::X,
                false,
                "Cancel",
                move |_window, cx| {
                    cancel_view.update(cx, |this, cx| {
                        this.state.pending_delete = None;
                        cx.notify();
                    });
                },
            ));
        } else {
            if !is_folder {
                buttons.push(row_action(
                    SharedString::from(format!("download-{index}")),
                    group.clone(),
                    hidden,
                    IconName::Download,
                    false,
                    "Download",
                    move |_window, cx| spawn_download(download_view.clone(), index, cx),
                ));
            }
            buttons.push(row_action(
                SharedString::from(format!("rename-{index}")),
                group.clone(),
                hidden,
                IconName::Pencil,
                false,
                "Rename",
                move |window, cx| {
                    let file = rename_view.read(cx).state.files[index].clone();
                    rename_view.update(cx, |this, cx| {
                        this.state.close_move();
                        this.state.begin_rename(&file);
                        cx.notify();
                    });
                    let input = rename_view.read(cx).rename_input.clone();
                    seed_name_field(&input, &file.name, window, cx);
                },
            ));
            buttons.push(row_action(
                SharedString::from(format!("move-{index}")),
                group.clone(),
                hidden,
                IconName::FolderInput,
                false,
                "Move to",
                move |_window, cx| {
                    let file = move_view.read(cx).state.files[index].clone();
                    move_view.update(cx, |this, cx| {
                        this.state.close_rename();
                        this.state.begin_move(&file);
                        cx.notify();
                    });
                },
            ));
            buttons.push(row_action(
                SharedString::from(format!("delete-{index}")),
                group.clone(),
                hidden,
                IconName::Trash,
                true,
                "Delete",
                move |_window, cx| {
                    let message_id = ask_delete_view.read(cx).state.files[index].message_id;
                    ask_delete_view.update(cx, |this, cx| {
                        this.state.pending_delete = Some(message_id);
                        this.state.selected_file = Some(index);
                        cx.notify();
                    });
                },
            ));
        }

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
            .children(buttons)
            .into_any_element();

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
                let parent_id = file.parent.clone();
                move |event, window, cx| {
                    // A single click selects the row; a double click activates
                    // it, which is the split the original's `activateRow`
                    // describes. Activation is what asks for the drive's key,
                    // because an encrypted file cannot be read before it.
                    let activate = event.click_count() >= 2;
                    // The keys held with the click, which is how the original
                    // tells a plain pick from a range or a toggle.
                    let modifiers = event.modifiers();
                    // The rows on screen, which is the order a range
                    // selection follows.
                    let rows = visible_rows(&clicking, cx);
                    // A search result can live in any folder, so activating one
                    // goes to where it is instead of acting on it in place,
                    // which is the original's `openFileResult`.
                    let searching = clicking.read(cx).state.searching();
                    if searching {
                        clicking.update(cx, |this, cx| {
                            pick_row(&mut this.state, index, &modifiers, &rows);
                            this.state.pending_delete = None;
                            cx.notify();
                        });
                        if activate {
                            clicking.update(cx, |this, cx| {
                                let title = this
                                    .state
                                    .folder
                                    .first()
                                    .map(|crumb| crumb.name.clone())
                                    .unwrap_or_default();
                                let target = if is_folder {
                                    folder_id.clone()
                                } else {
                                    parent_id.clone()
                                };
                                this.state.reveal_folder(&target, &title);
                                if !is_folder {
                                    this.state.select_object(&folder_id);
                                }
                                cx.notify();
                            });
                            // The search box is part of the mode the reveal
                            // left, so it is emptied with it rather than left
                            // showing a query the list no longer answers.
                            clicking.update(cx, |this, cx| {
                                let input = this.search_input.clone();
                                input.update(cx, |state, cx| state.set_value("", window, cx));
                            });
                        }
                        return;
                    }
                    if is_folder {
                        // A folder is picked like anything else; a double
                        // click opens it, which is the split the original
                        // makes between selecting a row and activating it.
                        clicking.update(cx, |this, cx| {
                            pick_row(&mut this.state, index, &modifiers, &rows);
                            this.state.pending_delete = None;
                            cx.notify();
                        });
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
                        pick_row(&mut this.state, index, &modifiers, &rows);
                        this.state.pending_delete = None;
                        cx.notify();
                    });
                    if activate {
                        // A double click opens the file, the way the original
                        // does. A kind the viewer cannot show is saved
                        // instead, which is what the original's viewer does
                        // with the kinds it cannot open.
                        spawn_preview(clicking.clone(), index, cx);
                        // A row that is locked raises the unlock prompt instead
                        // of opening, so the caret goes straight into it and
                        // the passphrase can be typed without a second click.
                        if clicking
                            .read(cx)
                            .state
                            .vault
                            .as_ref()
                            .is_some_and(|vault| vault.prompt.is_some())
                        {
                            clicking.update(cx, |this, _cx| this.vault_focus_pending = true);
                        }
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
                    move |_window, cx| purge_trash_entry(purge_view.clone(), index, cx),
                )
            } else {
                row_action(
                    SharedString::from(format!("restore-{index}")),
                    group.clone(),
                    false,
                    IconName::Undo2,
                    false,
                    "Restore",
                    move |_window, cx| restore_trash_entry(restore_view.clone(), index, cx),
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
                    move |_window, cx| {
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
                    move |_window, cx| {
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

/// The bar that covers the table's header while rows are picked.
///
/// The original draws the count on the left and the actions on the right, so
/// the column labels step aside for it while a selection is live.
fn selection_bar(view: Entity<TDriveView>, count: usize) -> AnyElement {
    let label = if count == 1 {
        "1 selected".to_string()
    } else {
        format!("{count} selected")
    };
    let download_view = view.clone();
    let move_view = view.clone();
    let delete_view = view.clone();
    let clear_view = view.clone();
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .px(px(30.))
        .bg(theme::canvas())
        .child(
            div()
                .text_size(px(theme::TYPE_SM))
                .font_weight(FontWeight::BOLD)
                .text_color(theme::text())
                .child(label),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .child(
                    Button::new("selection-download")
                        .label("Download")
                        .on_click(move |_, _window, cx| {
                            download_selected(download_view.clone(), cx);
                        }),
                )
                .child(Button::new("selection-move").label("Move").on_click(
                    move |_, _window, cx| {
                        move_selected(move_view.clone(), cx);
                    },
                ))
                .child(
                    Button::new("selection-delete")
                        .danger()
                        .label("Delete")
                        .on_click(move |_, _window, cx| {
                            delete_selected(delete_view.clone(), cx);
                        }),
                )
                .child(
                    Button::new("selection-clear")
                        .ghost()
                        .label("Clear")
                        .on_click(move |_, _window, cx| {
                            clear_view.update(cx, |this, cx| {
                                this.state.clear_selection();
                                cx.notify();
                            });
                        }),
                ),
        )
        .into_any_element()
}

/// The rows the list is showing, in the order it shows them.
///
/// A shift-click takes the range between the anchor and a row, so the range
/// has to follow the order on screen rather than the order of the drive.
fn visible_rows(view: &Entity<TDriveView>, cx: &App) -> Vec<usize> {
    view.read(cx)
        .rows_cache
        .borrow()
        .as_ref()
        .map(|cache| cache.rows.as_ref().clone())
        .unwrap_or_default()
}

/// Pick a row the way the original's selection does: a plain click takes it
/// alone, a modified click adds it to the set or takes it out again, and a
/// shift-click takes everything between the anchor and the row.
fn pick_row(state: &mut AppState, index: usize, modifiers: &gpui::Modifiers, rows: &[usize]) {
    if modifiers.shift {
        state.select_range(rows, index);
    } else if modifiers.control || modifiers.platform {
        state.toggle_selected(index);
    } else {
        state.select_only(index);
    }
}

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
            let armed = button_view.read(cx).state.confirm_empty_trash;
            if armed {
                empty_trash(button_view.clone(), cx);
            } else {
                button_view.update(cx, |this, cx| {
                    this.state.confirm_empty_trash = true;
                    cx.notify();
                });
            }
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

/// The dimmed backdrop every dialog sits on, matching the original's
/// `ModalShell`: one centred card over the shell, with the shell still visible
/// behind it.
fn modal_scrim(card: impl IntoElement) -> AnyElement {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::rgba(0x05070dcc))
        .child(card)
        .into_any_element()
}

/// The card inside [`modal_scrim`]: the width, padding and border every dialog
/// in this port shares.
fn modal_card(width: f32) -> gpui::Div {
    div()
        .w(px(width))
        .flex()
        .flex_col()
        .gap(px(14.))
        .p(px(24.))
        .rounded(px(12.))
        .bg(theme::surface_1())
        .border_1()
        .border_color(theme::border_soft())
}

/// A dialog's title, in the weight the original's `ModalShell` gives it.
fn modal_title(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .text_size(px(theme::TYPE_MD))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text())
        .child(text.into())
}

/// A bar that shows that something is happening and how far it has got.
///
/// The drive's size is not known before it is read, so the bar is a slow
/// sweep rather than a fraction: it says that the work is real and moving.
fn progress_bar(sweep: f32) -> impl IntoElement {
    // The drive's size is not known before it is read, so the bar is a sweep
    // rather than a fraction: it says the work is real and moving.
    let position = (sweep * 0.7).clamp(0.0, 0.7);
    div()
        .h(px(4.))
        .w_full()
        .rounded(px(2.))
        .overflow_hidden()
        .bg(theme::surface_3())
        .child(
            div()
                .absolute()
                .top_0()
                .h_full()
                .w(px(120.))
                .left(px(position * 1000.))
                .rounded(px(2.))
                .bg(theme::accent()),
        )
}

/// A number with the thousands marked out, the way the original writes them.
fn thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// A dialog's one-line subtitle.
fn modal_subtitle(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .text_size(px(theme::TYPE_SM))
        .text_color(theme::text_muted())
        .child(text.into())
}

/// Fill a name field that was just put on screen and put the caret in it, so
/// the name can be replaced without a second click.
fn seed_name_field(input: &Entity<InputState>, name: &str, window: &mut Window, cx: &mut App) {
    let input = input.clone();
    let name = name.to_string();
    input.update(cx, |state, cx| {
        state.set_value(name, window, cx);
        state.focus(window, cx);
    });
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
    /// Create the drive's encryption config from the setup form.
    SubmitEncryptionSetup,
    /// Take the name in the rename dialog.
    SubmitRename,
    /// Create the folder named in the "New folder" dialog.
    SubmitFolder,
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
        LoginAction::SubmitEncryptionSetup => submit_encryption_setup(view, cx),
        LoginAction::SubmitRename => commit_rename(view, cx),
        LoginAction::SubmitFolder => commit_folder(view, cx),
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

    let creds = tdrive_rs::models::Credentials { api_id, api_hash };
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
/// Put a freshly read drive on screen: its files, its trash and its vault.
///
/// Opening a drive and re-reading one after a write both draw these three
/// things, so they share this; the breadcrumb, the view and the key belong to
/// whoever asked for the read.
fn apply_contents(state: &mut AppState, title: &str, result: Result<DriveContents, String>) {
    state.files_loading = false;
    match result {
        Ok(contents) => {
            let count = contents.files.len();
            state.files = contents.files;
            // The rows the selection named may not be there any more.
            state.clear_selection();
            // The drive's own trash is part of what was read, so the Trash view
            // shows what the *drive* deleted, not only what this session did.
            state.trash = contents
                .trash
                .into_iter()
                .map(|entry| TrashEntry {
                    deleted_at: DateTime::from_timestamp(entry.deleted_at, 0)
                        .unwrap_or_else(Utc::now),
                    origin: title.to_string(),
                    file: entry.file,
                })
                .collect();
            // The key belongs to the drive, so a drive that has one asks for
            // it; one that does not never will.
            state.set_vault(contents.vault);
            state.touch();
            state.status = format!("{count} items in {title}");
        }
        Err(message) => {
            state.status = "Could not open drive".to_string();
            state.error = Some(message);
        }
    }
}

/// Open a drive, or re-read the one already open.
///
/// `refresh` keeps the breadcrumb, the view and the unlock key the user already
/// has, and is what a write asks for afterwards: the projection in the drive is
/// the authority on what an operation did, so the list is rebuilt from it
/// rather than patched here, and a write the drive refused cannot leave the
/// screen claiming otherwise.
fn spawn_load_drive(view: Entity<TDriveView>, index: usize, refresh: bool, cx: &mut App) {
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
        if !refresh {
            view.update(cx, |this, cx| {
                this.state.open_drive(index);
                this.state.set_view(ViewMode::Drive);
                this.state.files_loading = false;
                this.state.status = format!("{title} (demo data)");
                cx.notify();
            });
        }
        return;
    };

    view.update(cx, |this, cx| {
        if !refresh {
            this.state.open_drive(index);
            this.state.set_view(ViewMode::Drive);
            this.state.files.clear();
            this.state.vault = None;
        }
        this.state.pending_delete = None;
        this.state.pending_purge = None;
        this.state.confirm_empty_trash = false;
        this.state.files_loading = true;
        this.state.load_progress = Some(tele::last_progress());
        this.state.status = if refresh {
            "Reloading...".to_string()
        } else {
            format!("Opening {title}...")
        };
        this.state.error = None;
        cx.notify();
    });

    // A drive that was read before resumes from the snapshot it left behind:
    // the replay carries on from there and only the messages that arrived since
    // are fetched. `TDRIVE_FULL_LOAD=1` (and the Reload action) reads the whole
    // history again instead.
    let resume = if std::env::var_os("TDRIVE_FULL_LOAD").is_some() {
        None
    } else {
        cache::load(drive_id).map(|snapshot| (snapshot.newest_message_id, snapshot.state))
    };

    // The hook's own copy of the view, taken before the load's task takes it.
    let hook_view = view.clone();
    // A second task watches the read and keeps the shell told where it is, so
    // a long read is a bar that moves rather than a message that never changes.
    let watch_view = view.clone();
    let watch_drive = drive_id;
    let mut progress = tele::progress_receiver();
    cx.spawn(async move |cx| {
        // The read says where it is, and the shell draws it. The read is on a
        // thread of its own, so this is how the two talk.
        while progress.changed().await.is_ok() {
            let current = *progress.borrow();
            let done = cx.update(|cx| {
                watch_view.update(cx, |this, cx| {
                    if this.state.active_dialog().map(|d| d.id) != Some(watch_drive) {
                        return true;
                    }
                    this.state.load_progress = Some(current);
                    // The bar moves whether or not the count has: a read that
                    // is working on one long message still looks like work.
                    this.state.load_sweep = (this.state.load_sweep + 0.04) % 1.0;
                    cx.notify();
                    this.state.files_loading
                })
            });
            if done {
                return;
            }
        }
    })
    .detach();
    // The read runs on a thread of its own.
    //
    // It waits on the network for a long time — a large drive takes a minute
    // or more, with pauses when Telegram asks one — and a read that held the
    // shell's own background thread would stop the shell's timers with it, so
    // the bar that says where the read is would never move. The thread
    // carries the result back over a channel, and the shell waits on that.
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        match tele::load_drive(&client, peer_ref, resume) {
            Ok((contents, state, newest)) => {
                // Store what was read so the next launch can resume. A failed
                // write costs a slower load, never a wrong one.
                let _ = cache::save(&cache::Snapshot {
                    version: cache::VERSION,
                    drive_id,
                    newest_message_id: newest,
                    state,
                });
                let _ = sender.send(Ok(contents));
            }
            Err(message) => {
                let _ = sender.send(Err(message));
            }
        }
    });
    cx.spawn(async move |cx| {
        let result = match receiver.await {
            Ok(result) => result,
            Err(_) => Err("the read stopped".to_string()),
        };
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                // Still the drive this request was started for?
                if this.state.active_dialog().map(|d| d.id) != Some(drive_id) {
                    return;
                }
                apply_contents(&mut this.state, &title, result);
                cx.notify();
            });
            // A development aid: `TDRIVE_UPLOAD=<path>` sends that file to the
            // drive once it has been read, so the upload path can be walked
            // where there is no file dialog to pick a file with.
            maybe_upload_hook(hook_view.clone(), cx);
        });
    })
    .detach();
}

fn spawn_open_drive_inner(view: Entity<TDriveView>, index: usize, cx: &mut App) {
    spawn_load_drive(view, index, false, cx);
}

/// Write operations to the active drive, then re-read it.
///
/// Every mutation in the original leaves the same trace: an operation message
/// whose header the projection reads back. Nothing is applied to the list here,
/// so what is on screen afterwards is what the drive actually holds.
///
/// `purge` names trashed objects whose bytes should be destroyed as well, which
/// is the second half of a purge: the marker hides the namespace and the same
/// client deletes the bodies, exactly as the original's hard-delete job does.
fn spawn_write(
    view: Entity<TDriveView>,
    ops: Vec<Op>,
    purge: Vec<String>,
    done: String,
    cx: &mut App,
) {
    spawn_write_keeping_key(view, ops, purge, done, None, cx);
}

/// [`spawn_write`], plus a key to hold on to once the re-read lands.
///
/// Setting up encryption writes the config and then re-reads the drive, which
/// replaces the (previously absent) material with the new one. The key the user
/// just chose belongs to that material, so it is put back after the read.
fn spawn_write_keeping_key(
    view: Entity<TDriveView>,
    ops: Vec<Op>,
    purge: Vec<String>,
    done: String,
    keep_key: Option<(VaultMaterial, [u8; 32])>,
    cx: &mut App,
) {
    let (client, peer_ref, drive_id, title) = {
        let this = view.read(cx);
        let Some(dialog) = this.state.active_dialog() else {
            return;
        };
        (
            this.state.tele.clone(),
            dialog.peer_ref,
            dialog.id,
            dialog.title.clone(),
        )
    };

    view.update(cx, |this, cx| {
        this.state.pending_delete = None;
        this.state.pending_purge = None;
        this.state.confirm_empty_trash = false;
        this.state.close_rename();
        this.state.close_move();
        this.state.pending_folder = false;
        this.state.files_loading = true;
        this.state.status = "Saving...".to_string();
        this.state.error = None;
        cx.notify();
    });

    // Without a session there is nothing to write to. The demo walks its own
    // local half of the flow before it gets here.
    let Some(client) = client else {
        return;
    };

    // The operations about to be written are messages the drive has not
    // replayed yet, so the read starts from the snapshot and picks them up.
    let resume = cache::load(drive_id).map(|snapshot| (snapshot.newest_message_id, snapshot.state));

    cx.spawn(async move |cx| {
        let result = cx
            .background_spawn({
                let client = client.clone();
                async move {
                    for op in &ops {
                        tele::send_op(&client, peer_ref, op)?;
                    }
                    if !purge.is_empty() {
                        // The bodies a purge destroys are read from the snapshot
                        // the way the drive reads them from its own plan: the
                        // messages the trashed files' bytes live in.
                        let bodies = cache::load(drive_id)
                            .map(|snapshot| tele::purged_bodies(&snapshot.state, &purge))
                            .unwrap_or_default();
                        tele::delete_messages(&client, peer_ref, &bodies)?;
                    }
                    let (contents, state, newest) = tele::load_drive(&client, peer_ref, resume)?;
                    let _ = cache::save(&cache::Snapshot {
                        version: cache::VERSION,
                        drive_id,
                        newest_message_id: newest,
                        state,
                    });
                    Ok::<_, String>(contents)
                }
            })
            .await;
        let resume_upload = cx.update(|cx| {
            view.update(cx, |this, cx| {
                // The row may have moved, or the user may have moved to another
                // drive, while the request was in flight.
                if this.state.active_dialog().map(|d| d.id) != Some(drive_id) {
                    return false;
                }
                let resume = match result {
                    Ok(contents) => {
                        apply_contents(&mut this.state, &title, Ok(contents));
                        if let Some((material, key)) = keep_key.as_ref() {
                            this.state.remember_vault_key(material, *key);
                            clear_thumbnail_cache();
                        }
                        this.state.close_encryption_setup();
                        this.state.status = done;
                        // An upload that was waiting for the key this write
                        // has just made available is sent now.
                        this.state.pending_upload.is_some()
                    }
                    Err(message) => {
                        this.state.files_loading = false;
                        // A failure that belongs to a form says so inside the
                        // form, so the user can fix it without reopening it.
                        if let Some(setup) = this.state.encryption_setup.as_mut() {
                            setup.working = false;
                            setup.error = Some(message);
                        } else {
                            this.state.status = "Could not save".to_string();
                            this.state.error = Some(message);
                        }
                        false
                    }
                };
                cx.notify();
                resume
            })
        });
        if resume_upload {
            cx.update(|cx| advance_upload(view, cx));
        }
    })
    .detach();
}

/// Put the caret in the passphrase field when the unlock prompt is up.
///
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
            this.state
                .request_vault_for(VaultResume::Open, Some(message_id));
            this.vault_focus_pending = true;
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
        // A batch continues with the next file, so a handful of them arrive
        // one after another rather than all at once.
        cx.update(|cx| next_download(view.clone(), cx));
    })
    .detach();
}

fn spawn_upload(view: Entity<TDriveView>, cx: &mut App) {
    let has_session = {
        let this = view.read(cx);
        this.state.tele.is_some() && this.state.active_dialog().is_some()
    };
    if !has_session {
        // An upload has nothing to write to without a session, so say so
        // rather than leaving the button looking inert.
        view.update(cx, |this, cx| {
            this.state.status = "Connect to Telegram to upload".to_string();
            cx.notify();
        });
        return;
    }

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
        // Every upload to My Drive goes through the options modal, which is
        // where "Encrypt before upload" lives, exactly as the original asks it.
        // A shared drive has no such choice: its files are stored as they are.
        let personal = cx.update(|cx| {
            view.read(cx)
                .state
                .active_dialog()
                .is_some_and(|drive| drive.kind == DriveKind::Personal)
        });
        if personal {
            cx.update(|cx| {
                view.update(cx, |this, cx| {
                    this.state.begin_upload(path, 1);
                    this.state.status = "Choose how to upload".to_string();
                    cx.notify();
                });
            });
        } else {
            cx.update(|cx| spawn_upload_file(view, path, None, cx));
        }
    })
    .detach();
}

/// Continue the pending upload now that its encryption is settled.
///
/// A plain batch goes straight out. An encrypted one needs the drive's key: a
/// drive that has a config asks for its password, and a drive with no config
/// yet is sent to the setup form, because encrypting requires creating one.
fn advance_upload(view: Entity<TDriveView>, cx: &mut App) {
    let (pending, key, has_config, has_session) = {
        let this = view.read(cx);
        let Some(pending) = this.state.pending_upload.clone() else {
            return;
        };
        (
            Some(pending),
            this.state.vault.as_ref().and_then(|vault| vault.key),
            this.state.vault.is_some(),
            this.state.tele.is_some() && this.state.active_dialog().is_some(),
        )
    };
    let Some(pending) = pending else {
        return;
    };
    if !has_session {
        view.update(cx, |this, cx| {
            this.state.cancel_upload();
            this.state.status = "Connect to Telegram to upload".to_string();
            cx.notify();
        });
        return;
    }

    if pending.encrypt && key.is_none() {
        view.update(cx, |this, cx| {
            if has_config {
                this.state.request_vault_for(VaultResume::Upload, None);
                this.vault_focus_pending = true;
                this.state.status = "Enter your encryption password".to_string();
            } else {
                this.state.open_encryption_setup();
                this.state.status = "Set an encryption password".to_string();
            }
            cx.notify();
        });
        return;
    }

    // The batch is ready to go, so it stops being pending before the send
    // starts: a second trigger must not upload the same file twice.
    view.update(cx, |this, _cx| this.state.pending_upload = None);
    spawn_upload_file(
        view,
        pending.path,
        if pending.encrypt { key } else { None },
        cx,
    );
}

/// The upload options modal's Continue: keep the radio choice and continue.
fn submit_upload_options(view: Entity<TDriveView>, cx: &mut App) {
    let accepted = {
        let this = view.read(cx);
        this.state.upload_options.is_some()
    };
    if !accepted {
        return;
    }
    view.update(cx, |this, cx| {
        this.state.confirm_upload_choice();
        cx.notify();
    });
    advance_upload(view, cx);
}

/// Send one chosen file into the folder on screen.
///
/// `key` is the drive's master key when the batch is to be encrypted, and
/// `None` when its bytes go to Telegram as they are.
fn spawn_upload_file(view: Entity<TDriveView>, path: PathBuf, key: Option<[u8; 32]>, cx: &mut App) {
    let (client, peer_ref, parent, drive_id) = {
        let this = view.read(cx);
        let (Some(client), Some(drive)) = (this.state.tele.clone(), this.state.active_dialog())
        else {
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
            drive.id,
        )
    };

    view.update(cx, |this, cx| {
        this.state.status = format!("Uploading {}...", path.display());
        cx.notify();
    });

    cx.spawn(async move |cx| {
        let label = path.display().to_string();
        // A file lands in the folder on screen and is named the way the
        // filesystem names it, because that name goes into the drive's own
        // metadata rather than being Telegram's business.
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "upload".to_string());
        let upload_path = path.clone();
        let result = cx
            .background_spawn(async move {
                let message_id =
                    tele::upload_file(&client, peer_ref, &upload_path, &parent, &name, key)?;
                // The picture of a file that was just sent is made from the
                // file that was just sent, so nothing is fetched to make it.
                // It is published beside the file, so the gallery draws the
                // picture from that little message instead of pulling the file
                // again.
                publish_derivative(&client, peer_ref, drive_id, message_id, &upload_path, key);
                Ok(message_id)
            })
            .await;
        let to_reload = cx.update(|cx| {
            view.update(cx, |this, cx| match result {
                Ok(_) => {
                    this.state.status = format!("Uploaded {label}");
                    cx.notify();
                    this.state.active_drive
                }
                Err(message) => {
                    this.state.status = "Upload failed".to_string();
                    this.state.error = Some(message);
                    cx.notify();
                    None
                }
            })
        });
        if let Some(index) = to_reload {
            // The upload stays where it was put: the list is re-read rather
            // than reopened, so the folder on screen is still the one the file
            // landed in.
            cx.update(|cx| spawn_load_drive(view, index, true, cx));
        }
    })
    .detach();
}

/// Send the file `TDRIVE_UPLOAD` names, once the drive is ready for it.
///
/// A development aid, so the upload path can be walked where there is no file
/// dialog: the file goes out encrypted when the drive is, which is what the
/// upload options would have chosen.
fn maybe_upload_hook(view: Entity<TDriveView>, cx: &mut App) {
    let Ok(path) = std::env::var("TDRIVE_UPLOAD") else {
        return;
    };
    let ready = {
        let this = view.read(cx);
        if this.upload_hook_done {
            return;
        }
        if this.state.active_dialog().is_none() || this.state.tele.is_none() {
            return;
        }
        // An encrypted drive needs its key before a file can go out sealed.
        let vault = this.state.vault.as_ref();
        if vault.is_some() && vault.and_then(|vault| vault.key).is_none() {
            return;
        }
        true
    };
    if !ready {
        return;
    }
    view.update(cx, |this, _cx| this.upload_hook_done = true);
    let key = view
        .read(cx)
        .state
        .vault
        .as_ref()
        .and_then(|vault| vault.key);
    spawn_upload_file(view, PathBuf::from(path), key, cx);
}

/// Make the drive's pictures into derivatives, one file at a time.
///
/// A derivative is a small picture published beside the file it was made
/// from. A file that has one costs a few kilobytes to draw; a file without one
/// costs the whole file, every time it is looked at. This job makes them, so
/// the gallery is cheap afterwards.
///
/// Making one costs a fetch of the file, so the job says what it is about to
/// do and the reader can stop it.
fn spawn_derivative_job(view: Entity<TDriveView>, cx: &mut App) {
    let (client, peer_ref, drive_id, key, todo) = {
        let this = view.read(cx);
        let (Some(client), Some(drive)) = (this.state.tele.clone(), this.state.active_dialog())
        else {
            return;
        };
        // An encrypted drive needs its key: a derivative of a sealed file is
        // sealed too, and the file has to be read to be made.
        let key = this.state.vault.as_ref().and_then(|vault| vault.key);
        let encrypted = this.state.vault.is_some();
        if encrypted && key.is_none() {
            view.update(cx, |this, cx| {
                this.state.request_vault_for(VaultResume::Nothing, None);
                this.vault_focus_pending = true;
                this.state.status = "Enter your encryption password".to_string();
                cx.notify();
            });
            return;
        }
        // Every picture without a derivative, and nothing else.
        let todo: Vec<RemoteFile> = this
            .state
            .files
            .iter()
            .filter(|file| {
                file.kind == FileKind::Image
                    && !file.renditions.iter().any(|entry| entry.is_thumbnail())
            })
            .cloned()
            .collect();
        (client, drive.peer_ref, drive.id, key, todo)
    };

    if todo.is_empty() {
        view.update(cx, |this, cx| {
            this.state.status = "Every picture already has a preview".to_string();
            cx.notify();
        });
        return;
    }

    let total = todo.len();
    view.update(cx, |this, cx| {
        this.state.derivative_job = Some(DerivativeJob {
            done: 0,
            total,
            current: String::new(),
            cancelled: false,
            running: true,
        });
        this.state.status = format!("Making previews for {total} pictures...");
        cx.notify();
    });

    cx.spawn(async move |cx| {
        for file in todo {
            // The reader may stop the job between two files, which is the
            // only place it can stop without leaving a half-made picture.
            let cancelled = cx.update(|cx| {
                view.read(cx)
                    .state
                    .derivative_job
                    .as_ref()
                    .is_some_and(|job| job.cancelled)
            });
            if cancelled {
                break;
            }

            let name = file.name.clone();
            cx.update(|cx| {
                view.update(cx, |this, cx| {
                    if let Some(job) = this.state.derivative_job.as_mut() {
                        job.current = name.clone();
                    }
                    this.state.status = format!("Making a preview of {name}...");
                    cx.notify();
                });
            });

            // The file is fetched, made into a picture, and thrown away; only
            // the picture is published.
            let bytes = {
                let client = client.clone();
                let peer_ref = peer_ref.clone();
                let file = file.clone();
                let key = key;
                cx.background_spawn(async move {
                    let staged = std::env::temp_dir()
                        .join(format!("tdrive-derivative-{}.bin", uuid::Uuid::new_v4()));
                    let messages = content_messages(&file);
                    let downloaded =
                        tele::download_file(&client, peer_ref, &messages, &staged, key);
                    let bytes = match downloaded {
                        Ok(()) => std::fs::read(&staged).ok(),
                        Err(_) => None,
                    };
                    let _ = std::fs::remove_file(&staged);
                    bytes
                })
                .await
            };

            if let Some(bytes) = bytes {
                if let Some(picture) = rendition::generate(&bytes, rendition::THUMBNAIL_EDGE) {
                    let mut descriptor = ops::Rendition {
                        channel_id: drive_id,
                        file_msg_id: i64::from(file.message_id),
                        content_msg_id: match file.content {
                            ContentRef::OpMessage => i64::from(file.message_id),
                            ContentRef::Message(id) => i64::from(id),
                            ContentRef::Parts(_) => 0,
                        },
                        upload_uuid: String::new(),
                        kind: ops::RENDITION_THUMBNAIL.to_string(),
                        version: 1,
                        size: 0,
                        plaintext_size: picture.jpeg.len() as i64,
                        width: picture.width as i32,
                        height: picture.height as i32,
                        encrypted: key.is_some(),
                    };
                    let payload = match key {
                        Some(key) => match rendition::encrypt(&picture.jpeg, &key, &descriptor) {
                            Ok(payload) => payload,
                            Err(_) => Vec::new(),
                        },
                        None => picture.jpeg.clone(),
                    };
                    if !payload.is_empty() {
                        descriptor.size = payload.len() as i64;
                        let client = client.clone();
                        let _ = cx
                            .background_spawn(async move {
                                tele::upload_rendition(&client, peer_ref, &descriptor, &payload)
                            })
                            .await;
                    }
                }
            }

            cx.update(|cx| {
                view.update(cx, |this, cx| {
                    if let Some(job) = this.state.derivative_job.as_mut() {
                        job.done += 1;
                    }
                    cx.notify();
                });
            });
        }

        // The drive is read again, so the pictures that now have a derivative
        // are drawn from it.
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                let done = this
                    .state
                    .derivative_job
                    .as_ref()
                    .map(|job| job.done)
                    .unwrap_or_default();
                this.state.derivative_job = None;
                this.state.status = format!("Made {done} previews");
                cx.notify();
            });
        });
        let index = cx.update(|cx| view.read(cx).state.active_drive);
        if let Some(index) = index {
            cx.update(|cx| spawn_load_drive(view, index, true, cx));
        }
    })
    .detach();
}

/// Publish the picture of a file beside the file it was made from.
///
/// The picture is a small derivative: a few tens of kilobytes, made from the
/// file's own bytes. It is published as its own message, the way the original
/// publishes one, so the gallery draws the picture from that little message
/// instead of pulling the whole file again — and so the original app, which
/// reads the same kind of message, draws it too.
///
/// A file that is not a picture has no picture to publish.
fn publish_derivative(
    client: &TeleClient,
    peer_ref: grammers_session::types::PeerRef,
    drive_id: i64,
    file_message_id: i32,
    path: &Path,
    key: Option<[u8; 32]>,
) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let Some(picture) = rendition::generate(&bytes, rendition::THUMBNAIL_EDGE) else {
        return;
    };
    let encrypted = key.is_some();
    let mut descriptor = ops::Rendition {
        channel_id: drive_id,
        file_msg_id: i64::from(file_message_id),
        // The file's own message holds its bytes, so that is the message the
        // derivative is bound to.
        content_msg_id: i64::from(file_message_id),
        upload_uuid: String::new(),
        kind: ops::RENDITION_THUMBNAIL.to_string(),
        version: 1,
        size: 0,
        plaintext_size: picture.jpeg.len() as i64,
        width: picture.width as i32,
        height: picture.height as i32,
        encrypted,
    };
    let payload = match key {
        Some(key) => match rendition::encrypt(&picture.jpeg, &key, &descriptor) {
            Ok(payload) => payload,
            Err(_) => return,
        },
        None => picture.jpeg.clone(),
    };
    descriptor.size = payload.len() as i64;
    // A derivative that cannot be published is not worth failing the upload
    // over: the file itself is already in the drive.
    let _ = tele::upload_rendition(client, peer_ref, &descriptor, &payload);
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
        let (resume, unlocked) = cx.update(|cx| {
            view.update(cx, |this, cx| {
                let Some(vault) = this.state.vault.as_mut() else {
                    return (None, false);
                };
                vault.working = false;
                let unlocked = result.is_ok();
                let resume = match result {
                    Ok(key) => {
                        vault.key = Some(key);
                        vault.error = None;
                        vault.prompt.take()
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
                if unlocked {
                    // Ciphertext cached while locked is now stale.
                    clear_thumbnail_cache();
                }
                if this
                    .state
                    .vault
                    .as_ref()
                    .is_some_and(|vault| vault.unlocked())
                {
                    this.state.status = "Encrypted files unlocked".to_string();
                }
                cx.notify();
                (resume, unlocked)
            })
        });

        if unlocked {
            // The drive can take a sealed file now.
            let hook_view = view.clone();
            cx.update(|cx| maybe_upload_hook(hook_view, cx));
        }

        // Unlocking resumes whatever asked for the key: a download of a row, an
        // upload whose encryption was waiting on it, or a rename or a move the
        // drive refused while the file was locked. A download's row is found by
        // message id so this stays correct even if the list moved underneath it.
        if let Some(prompt) = resume {
            match prompt.resume {
                VaultResume::Upload => cx.update(|cx| advance_upload(view, cx)),
                VaultResume::Rename => cx.update(|cx| {
                    let view = view.clone();
                    view.update(cx, |this, cx| {
                        if let Some(pending) = this.state.pending_rename.as_mut() {
                            pending.working = false;
                        }
                        cx.notify();
                    });
                    commit_rename(view, cx);
                }),
                VaultResume::Move => cx.update(|cx| {
                    let view = view.clone();
                    view.update(cx, |this, cx| {
                        if let Some(pending) = this.state.pending_move.as_mut() {
                            pending.working = false;
                        }
                        cx.notify();
                    });
                    commit_move(view, cx);
                }),
                VaultResume::Nothing => {}
                VaultResume::Open => {
                    if let Some(message_id) = prompt.message_id {
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
                }
                VaultResume::Preview => {
                    if let Some(message_id) = prompt.message_id {
                        cx.update(|cx| {
                            let index = view
                                .read(cx)
                                .state
                                .files
                                .iter()
                                .position(|file| file.message_id == message_id);
                            if let Some(index) = index {
                                spawn_preview(view, index, cx);
                            }
                        });
                    }
                }
            }
        }
    })
    .detach();
}

/// Create the drive's encryption config from the setup form.
///
/// The rules are the original's: at least eight characters, typed twice, and an
/// optional hint that travels in the clear. The config is one `encfg`
/// operation; the master key it wraps stays in memory so the files the user
/// uploads next are encrypted with it straight away.
fn submit_encryption_setup(view: Entity<TDriveView>, cx: &mut App) {
    let (password, confirm, hint) = {
        let this = view.read(cx);
        let Some(setup) = this.state.encryption_setup.as_ref() else {
            return;
        };
        if setup.working {
            return;
        }
        (
            this.encrypt_password_input.read(cx).value().to_string(),
            this.encrypt_confirm_input.read(cx).value().to_string(),
            this.encrypt_hint_input.read(cx).value().to_string(),
        )
    };

    let problem = if password.chars().count() < 8 {
        Some("Use at least 8 characters.")
    } else if password != confirm {
        Some("Passwords don\u{2019}t match.")
    } else {
        None
    };
    if let Some(problem) = problem {
        view.update(cx, |this, cx| {
            if let Some(setup) = this.state.encryption_setup.as_mut() {
                setup.error = Some(problem.to_string());
            }
            cx.notify();
        });
        return;
    }

    // Without a session, or with no drive open, there is nothing to write the
    // config to; the button that opens this form is only drawn with both.
    if view.read(cx).state.tele.is_none() || view.read(cx).state.active_dialog().is_none() {
        return;
    }

    view.update(cx, |this, cx| {
        if let Some(setup) = this.state.encryption_setup.as_mut() {
            setup.working = true;
            setup.error = None;
        }
        cx.notify();
    });

    cx.spawn(async move |cx| {
        // The KDF is deliberately slow, so the config is built off the UI
        // thread before anything is written to the drive.
        let created = cx
            .background_spawn(async move {
                let hint = hint.trim().to_string();
                crypto::create_vault(&password, &hint).map(|(material, key)| (material, key, hint))
            })
            .await;

        let (material, key, hint) = match created {
            Ok(created) => created,
            Err(error) => {
                cx.update(|cx| {
                    view.update(cx, |this, cx| {
                        if let Some(setup) = this.state.encryption_setup.as_mut() {
                            setup.working = false;
                            setup.error = Some(format!("Could not set up encryption: {error}"));
                        }
                        cx.notify();
                    });
                });
                return;
            }
        };

        let op = tele::enc_config_op(&material, &hint);
        cx.update(|cx| {
            spawn_write_keeping_key(
                view,
                vec![op],
                Vec::new(),
                "Encryption is on for this drive".to_string(),
                Some((material, key)),
                cx,
            );
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

/// Delete a row: the drive's own trash, not a Telegram deletion.
///
/// The original publishes a trash operation and stops, which is what leaves the
/// file restorable for its retention window; the bytes stay where they are until
/// the object is purged. Deleting the messages instead, as this port used to,
/// destroys a file the user can still see in the trash.
fn spawn_delete(view: Entity<TDriveView>, index: usize, cx: &mut App) {
    let (client, file, demo) = {
        let this = view.read(cx);
        let Some(file) = this.state.files.get(index).cloned() else {
            return;
        };
        (this.state.tele.clone(), file, this.state.demo)
    };

    // Without a session there is nothing to delete on Telegram, but the demo
    // still walks the local half of the flow so the trash is reachable.
    if client.is_none() {
        if demo {
            view.update(cx, |this, cx| {
                let title = this
                    .state
                    .active_dialog()
                    .map(|drive| drive.title.clone())
                    .unwrap_or_default();
                this.state.pending_delete = None;
                this.state.status =
                    match move_file_to_trash(&mut this.state, file.message_id, &title) {
                        Some(name) => format!("Moved {name} to trash"),
                        None => "Nothing to delete".to_string(),
                    };
                cx.notify();
            });
        }
        return;
    }

    let op = ops::trash_op(
        &file.object_id,
        file.revision,
        Utc::now().timestamp(),
        PURGE_WINDOW_DAYS * 86_400,
    );
    let name = file.name.clone();
    spawn_write(
        view,
        vec![op],
        Vec::new(),
        format!("Moved {name} to trash"),
        cx,
    );
}

/// Take the name in the rename dialog.
///
/// The drive refuses to rename an encrypted file while its key is missing, so
/// a locked row asks for the passphrase first and comes back here once it is
/// known. A name that is empty never gets this far: the button that calls it is
/// dimmed.
fn commit_rename(view: Entity<TDriveView>, cx: &mut App) {
    let (target, name, demo, key, has_client) = {
        let this = view.read(cx);
        let Some(target) = this.state.pending_rename.clone() else {
            return;
        };
        if target.working {
            return;
        }
        (
            target,
            this.rename_input.read(cx).value().trim().to_string(),
            this.state.demo,
            this.state.vault.as_ref().and_then(|vault| vault.key),
            this.state.tele.is_some(),
        )
    };
    if name.is_empty() {
        return;
    }

    let encrypted = view
        .read(cx)
        .state
        .files
        .iter()
        .any(|file| file.message_id == target.message_id && file.encrypted);
    if encrypted && key.is_none() {
        view.update(cx, |this, cx| {
            this.state
                .request_vault_for(VaultResume::Rename, Some(target.message_id));
            this.vault_focus_pending = true;
            if let Some(pending) = this.state.pending_rename.as_mut() {
                pending.working = true;
            }
            cx.notify();
        });
        return;
    }

    // Without a session there is nothing to write to, but the demo still walks
    // the local half of the flow.
    if !has_client {
        if demo {
            view.update(cx, |this, cx| {
                this.state.rename_local(&target.object_id, &name);
                this.state.pending_rename = None;
                this.state.status = format!("Renamed to {name}");
                cx.notify();
            });
        }
        return;
    }

    view.update(cx, |this, cx| {
        if let Some(pending) = this.state.pending_rename.as_mut() {
            pending.working = true;
        }
        cx.notify();
    });
    let op = ops::rename_op(&target.object_id, &name);
    spawn_write(view, vec![op], Vec::new(), format!("Renamed to {name}"), cx);
}

/// Take the destination the move dialog is pointing at.
fn commit_move(view: Entity<TDriveView>, cx: &mut App) {
    let (items, destination, destination_name, demo, key, has_client) = {
        let this = view.read(cx);
        let Some(target) = this.state.pending_move.clone() else {
            return;
        };
        if target.working || !this.state.move_available() {
            return;
        }
        let (destination, destination_name) = this
            .state
            .move_path
            .last()
            .map(|crumb| (crumb.id.clone(), crumb.name.clone()))
            .unwrap_or_default();
        (
            target.items,
            destination,
            destination_name,
            this.state.demo,
            this.state.vault.as_ref().and_then(|vault| vault.key),
            this.state.tele.is_some(),
        )
    };
    if items.is_empty() {
        return;
    }

    // The drive refuses to move an encrypted file while its key is missing, so
    // a locked row asks for the passphrase and the move picks up again once
    // the key is known.
    let locked = items.iter().find(|item| item.encrypted);
    if locked.is_some() && key.is_none() {
        let message_id = items.first().map(|item| item.message_id);
        view.update(cx, |this, cx| {
            this.state.request_vault_for(VaultResume::Move, message_id);
            this.vault_focus_pending = true;
            if let Some(pending) = this.state.pending_move.as_mut() {
                pending.working = true;
            }
            cx.notify();
        });
        return;
    }

    if !has_client {
        if demo {
            view.update(cx, |this, cx| {
                for item in &items {
                    this.state.move_local(&item.object_id, &destination);
                }
                this.state.close_move();
                this.state.status = format!("Moved {} to {destination_name}", describe(&items));
                cx.notify();
            });
        }
        return;
    }

    view.update(cx, |this, cx| {
        if let Some(pending) = this.state.pending_move.as_mut() {
            pending.working = true;
        }
        cx.notify();
    });
    // Each row is moved on its own, the way the original's move writes one
    // operation per item.
    let ops: Vec<Op> = items
        .iter()
        .map(|item| ops::move_op(&item.object_id, &destination))
        .collect();
    spawn_write(
        view,
        ops,
        Vec::new(),
        format!("Moved {} to {destination_name}", describe(&items)),
        cx,
    );
}

/// How a batch of rows is named in a message: the name of the one row, or the
/// count of the many, the way the original names them.
fn describe(items: &[tdrive_rs::models::MoveItem]) -> String {
    match items {
        [one] => one.name.clone(),
        many => format!("{} items", many.len()),
    }
}

/// Create the folder named in the "New folder" dialog.
///
/// The object id is chosen here rather than taken from the message the
/// operation travels in, which is the original's `d:<uuid>`: it stays the
/// folder's identity for the whole life of the drive.
fn commit_folder(view: Entity<TDriveView>, cx: &mut App) {
    let (name, parent, demo, has_client) = {
        let this = view.read(cx);
        if !this.state.pending_folder {
            return;
        }
        (
            this.folder_input.read(cx).value().trim().to_string(),
            this.state.current_folder().to_string(),
            this.state.demo,
            this.state.tele.is_some(),
        )
    };
    if name.is_empty() {
        return;
    }

    if !has_client {
        if demo {
            view.update(cx, |this, cx| {
                this.state.mkdir_local(&parent, &name);
                this.state.pending_folder = false;
                this.state.status = format!("Created {name}");
                cx.notify();
            });
        }
        return;
    }

    let object_id = ops::new_folder_id();
    let op = ops::mkdir_op(&object_id, &parent, &name);
    spawn_write(view, vec![op], Vec::new(), format!("Created {name}"), cx);
}

/// Download every picked row, one after another.
///
/// The original queues a batch in its transfers panel; this walks the same
/// list one file at a time, so a drive is never asked for a dozen files at
/// once.
fn download_selected(view: Entity<TDriveView>, cx: &mut App) {
    let rows = {
        let this = view.read(cx);
        this.state.selected_rows()
    };
    // A folder has no body of its own to fetch, so it is left out rather than
    // counted and then silently skipped.
    let rows: Vec<usize> = rows
        .into_iter()
        .filter(|index| {
            view.read(cx)
                .state
                .files
                .get(*index)
                .is_some_and(|file| !file.is_folder())
        })
        .collect();
    if rows.is_empty() {
        view.update(cx, |this, cx| {
            this.state.status = "Nothing to download".to_string();
            cx.notify();
        });
        return;
    }
    view.update(cx, |this, cx| {
        this.state.download_queue = rows[1..].to_vec();
        this.state.status = format!("Downloading {} files", rows.len());
        cx.notify();
    });
    spawn_download(view, rows[0], cx);
}

/// The next file of a batch download, if there is one left.
fn next_download(view: Entity<TDriveView>, cx: &mut App) {
    let next = {
        let this = view.read(cx);
        this.state.download_queue.first().copied()
    };
    let Some(index) = next else {
        return;
    };
    view.update(cx, |this, cx| {
        if !this.state.download_queue.is_empty() {
            this.state.download_queue.remove(0);
        }
        cx.notify();
    });
    spawn_download(view, index, cx);
}

/// Move every picked row, through the picker a single row uses.
fn move_selected(view: Entity<TDriveView>, cx: &mut App) {
    let files = {
        let this = view.read(cx);
        this.state
            .selected_rows()
            .into_iter()
            .filter_map(|index| this.state.files.get(index).cloned())
            .collect::<Vec<_>>()
    };
    if files.is_empty() {
        return;
    }
    view.update(cx, |this, cx| {
        this.state.close_rename();
        this.state.begin_move_many(&files);
        cx.notify();
    });
}

/// Move every picked row to the trash.
///
/// Each row is its own operation, the way the original's bulk delete writes
/// one delete per item rather than one operation naming them all.
fn delete_selected(view: Entity<TDriveView>, cx: &mut App) {
    let (client, files, demo) = {
        let this = view.read(cx);
        (
            this.state.tele.clone(),
            this.state
                .selected_rows()
                .into_iter()
                .filter_map(|index| this.state.files.get(index).cloned())
                .collect::<Vec<_>>(),
            this.state.demo,
        )
    };
    if files.is_empty() {
        return;
    }
    let count = files.len();
    if client.is_none() {
        if demo {
            view.update(cx, |this, cx| {
                let title = this
                    .state
                    .active_dialog()
                    .map(|drive| drive.title.clone())
                    .unwrap_or_default();
                for file in &files {
                    move_file_to_trash(&mut this.state, file.message_id, &title);
                }
                this.state.clear_selection();
                this.state.status = format!("Moved {count} items to trash");
                cx.notify();
            });
        }
        return;
    }
    let now = Utc::now().timestamp();
    let ops: Vec<Op> = files
        .iter()
        .map(|file| {
            ops::trash_op(
                &file.object_id,
                file.revision,
                now,
                PURGE_WINDOW_DAYS * 86_400,
            )
        })
        .collect();
    view.update(cx, |this, cx| {
        this.state.clear_selection();
        cx.notify();
    });
    spawn_write(
        view,
        ops,
        Vec::new(),
        format!("Moved {count} items to trash"),
        cx,
    );
}

/// Open a file for looking at, the way the original's viewer does.
///
/// A picture or a text file is fetched and shown; anything else is saved
/// instead, the way the original's viewer refuses the kinds it cannot open.
fn spawn_preview(view: Entity<TDriveView>, index: usize, cx: &mut App) {
    let (client, peer_ref, drive_id, file, key) = {
        let this = view.read(cx);
        let Some(drive) = this.state.active_dialog().cloned() else {
            return;
        };
        let Some(file) = this.state.files.get(index).cloned() else {
            return;
        };
        (
            this.state.tele.clone(),
            drive.peer_ref,
            drive.id,
            file,
            this.state.vault.as_ref().and_then(|vault| vault.key),
        )
    };

    // The viewer can show a picture or a text file; the rest is saved.
    let Some(kind) = preview_kind(&file.name) else {
        spawn_download(view, index, cx);
        return;
    };

    // An encrypted file cannot be read without the drive's key, so ask for it
    // first; a successful unlock resumes the preview by itself.
    if file.encrypted && key.is_none() {
        view.update(cx, |this, cx| {
            this.state
                .request_vault_for(VaultResume::Preview, Some(file.message_id));
            this.vault_focus_pending = true;
            this.state.status = format!("{} is encrypted", file.name);
            cx.notify();
        });
        return;
    }

    let Some(client) = client else {
        view.update(cx, |this, cx| {
            this.state.status = "Connect to Telegram to open the file".to_string();
            cx.notify();
        });
        return;
    };

    let name = file.name.clone();
    let meta = format!(
        "{} · {}",
        file.kind.label(&name),
        human_size(file.display_size())
    );
    let messages = content_messages(&file);
    let dest = preview_path(&file);
    let key_for_task = if file.encrypted { key } else { None };
    view.update(cx, |this, cx| {
        this.state.status = format!("Opening {name}...");
        cx.notify();
    });

    cx.spawn(async move |cx| {
        let dest_for_task = dest.clone();
        let result = cx
            .background_spawn(async move {
                tele::download_file(&client, peer_ref, &messages, &dest_for_task, key_for_task)
            })
            .await;
        // A text file is read once here, so the viewer does not read it again
        // on every frame. A long file is cut short rather than filling the
        // screen with a wall of text.
        let text = match (kind, &result) {
            (PreviewKind::Text, Ok(())) => std::fs::read_to_string(&dest).ok().map(|body| {
                let mut text: String = body.chars().take(TEXT_PREVIEW_CHARS).collect();
                if body.chars().count() > TEXT_PREVIEW_CHARS {
                    text.push_str("\n\n… the file is longer than this");
                }
                text
            }),
            _ => None,
        };
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                // A drive switch replaces the whole list, so the file that was
                // opened is only still there if the drive is.
                if this.state.active_dialog().map(|d| d.id) != Some(drive_id) {
                    return;
                }
                match result {
                    Ok(()) => {
                        this.state.preview = Some(Preview {
                            kind,
                            title: name.clone(),
                            meta,
                            path: dest.clone(),
                            text,
                        });
                        this.state.status = format!("Opened {name}");
                    }
                    Err(message) => {
                        this.state.status =
                            format!("Could not open the file: {}", truncate(&message, 80));
                        this.state.error = Some(message);
                    }
                }
                cx.notify();
            });
        });
    })
    .detach();
}

/// How much of a text file the viewer shows, so a long log does not fill
/// the screen.
const TEXT_PREVIEW_CHARS: usize = 200_000;

/// Where an opened file is kept, so the same file is not fetched twice.
fn preview_path(file: &RemoteFile) -> PathBuf {
    directories::ProjectDirs::from("", "", "tdrive")
        .map(|dirs| dirs.cache_dir().join("preview"))
        .unwrap_or_else(|| PathBuf::from(".tdrive-preview"))
        .join(format!("{}-{}", file.message_id, sanitize_name(&file.name)))
}

/// The side of the picture the gallery keeps, in pixels.
const GALLERY_PREVIEW_SIDE: u32 = 512;

/// The largest file the gallery will fetch to make a picture of.
///
/// A picture of an encrypted file costs the whole file, because the bytes on
/// Telegram are ciphertext and carry no rendition of their own. This is the
/// line past which the gallery says the file is too large rather than
/// spending the reader's connection on it.
const GALLERY_PREVIEW_LIMIT: i64 = 32 * 1024 * 1024;

/// Fetch a picture for the gallery, and keep only the small one.
///
/// The full file is thrown away once the picture is made, so the cache holds
/// pictures rather than a second copy of the drive.
fn spawn_gallery_preview(view: Entity<TDriveView>, index: usize, cx: &mut App) {
    let (client, peer_ref, drive_id, file, key) = {
        let this = view.read(cx);
        let Some(drive) = this.state.active_dialog().cloned() else {
            return;
        };
        let Some(file) = this.state.files.get(index).cloned() else {
            return;
        };
        (
            this.state.tele.clone(),
            drive.peer_ref,
            drive.id,
            file,
            this.state.vault.as_ref().and_then(|vault| vault.key),
        )
    };

    if file.size > GALLERY_PREVIEW_LIMIT {
        view.update(cx, |this, cx| {
            this.state.status = format!("{} is too large to preview", file.name);
            cx.notify();
        });
        return;
    }

    let Some(client) = client else {
        view.update(cx, |this, cx| {
            this.state.status = "Connect to Telegram to open the file".to_string();
            cx.notify();
        });
        return;
    };

    let name = file.name.clone();
    let message_id = file.message_id;
    let thumb = thumb_path(drive_id, message_id);
    // The body is fetched beside the picture and deleted once the picture is
    // made, so a large file is never kept.
    let staged = thumb.with_extension("tdrive-body");
    let messages = content_messages(&file);
    let key_for_task = if file.encrypted { key } else { None };
    view.update(cx, |this, cx| {
        this.state.gallery_downloads.insert(message_id);
        this.state.status = format!("Fetching {name}...");
        cx.notify();
    });

    cx.spawn(async move |cx| {
        let staged_for_task = staged.clone();
        let result = cx
            .background_spawn(async move {
                tele::download_file(&client, peer_ref, &messages, &staged_for_task, key_for_task)
            })
            .await;
        // The picture is made from the file and the file is thrown away, so
        // what the cache keeps is a picture and not a copy of the drive.
        let made = match &result {
            Ok(()) => write_thumbnail(&staged, &thumb),
            Err(message) => Err(message.clone()),
        };
        let _ = std::fs::remove_file(&staged);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.state.gallery_downloads.remove(&message_id);
                if this.state.active_dialog().map(|d| d.id) != Some(drive_id) {
                    return;
                }
                match made {
                    Ok(()) => {
                        this.state.status = format!("Loaded {name}");
                    }
                    Err(message) => {
                        this.state.status = format!("Could not preview {name}");
                        this.state.error = Some(message);
                    }
                }
                cx.notify();
            });
        });
    })
    .detach();
}

/// Make a small picture from a file and write it out.
///
/// The picture is fitted into a square of the given size, so a big one costs
/// little to keep.
fn write_thumbnail(source: &Path, dest: &Path) -> Result<(), String> {
    let reader = image::ImageReader::open(source)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let picture = reader.decode().map_err(|e| e.to_string())?;
    let small = picture.thumbnail(GALLERY_PREVIEW_SIDE, GALLERY_PREVIEW_SIDE);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut writer = std::io::BufWriter::new(file);
    small
        .write_to(&mut writer, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// The viewer, drawn over the shell while a file is open.
fn render_preview(state: &AppState, close: impl Fn(&mut App) + 'static) -> Option<AnyElement> {
    let preview = state.preview.clone()?;
    let title = preview.title.clone();
    let meta = preview.meta.clone();
    let body = match preview.kind {
        // The picture is fitted into a box of a fixed height, so a large one
        // is scaled down rather than pushing the viewer off the screen, and a
        // small one keeps its own size.
        PreviewKind::Image => img(preview.path.clone())
            .w_full()
            .h(px(520.))
            .object_fit(ObjectFit::Contain)
            .into_any_element(),
        PreviewKind::Text => div()
            .id("preview-text")
            .w_full()
            .max_h(px(560.))
            .overflow_y_scroll()
            .font_family("monospace")
            .text_size(px(theme::TYPE_SM))
            .text_color(theme::text())
            .child(preview.text.unwrap_or_default())
            .into_any_element(),
    };
    Some(modal_scrim(
        modal_card(760.)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .child(
                        div()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(theme::TYPE_MD))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text())
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::TYPE_XS))
                                    .text_color(theme::text_muted())
                                    .child(meta),
                            ),
                    )
                    .child(
                        Button::new("preview-close")
                            .ghost()
                            .label("Close")
                            .on_click(move |_, _window, cx| close(cx)),
                    ),
            )
            .child(body),
    ))
}

/// Put a trashed item back in the drive it came from.
///
/// This port's trash is session-scoped: nothing was sent to Telegram to move
/// the item out of the drive, so restoring is a local move back into the list.
/// Put a trashed entry back, as the drive's own restore operation.
///
/// The folder it was deleted from may have gone since, in which case the drive
/// rehomes it at the root, and the name it had may be taken by now, in which
/// case a free one is chosen. Both are written into the operation, because the
/// drive cannot guess what this client meant.
fn restore_trash_entry(view: Entity<TDriveView>, index: usize, cx: &mut App) {
    let (client, entry, parent, name, demo) = {
        let this = view.read(cx);
        let Some(entry) = this.state.trash.get(index).cloned() else {
            return;
        };
        let mut parent = entry.file.parent.clone();
        if !parent.is_empty()
            && !this
                .state
                .files
                .iter()
                .any(|file| file.is_folder() && file.object_id == parent)
        {
            parent.clear();
        }
        let name = this
            .state
            .free_sibling_name(&parent, &entry.file.name, entry.file.is_folder());
        (
            this.state.tele.clone(),
            entry,
            parent,
            name,
            this.state.demo,
        )
    };

    if client.is_none() {
        // The demo has no drive to restore into, so the row returns locally.
        if demo {
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
        return;
    }

    let op = ops::restore_op(&entry.file.object_id, entry.file.revision, &parent, &name);
    spawn_write(
        view,
        vec![op],
        Vec::new(),
        format!("Restored {}", entry.file.name),
        cx,
    );
}

/// Drop a trashed item for good.
///
/// The marker hides it from the drive for ever, and the bodies its bytes live in
/// are deleted with it. Purging is the only path here that deletes Telegram
/// messages, which is why the drive requires the object to have been trashed
/// first.
fn purge_trash_entry(view: Entity<TDriveView>, index: usize, cx: &mut App) {
    let (client, entry, demo) = {
        let this = view.read(cx);
        let Some(entry) = this.state.trash.get(index).cloned() else {
            return;
        };
        (this.state.tele.clone(), entry, this.state.demo)
    };

    if client.is_none() {
        if demo {
            view.update(cx, |this, cx| {
                if index < this.state.trash.len() {
                    let entry = this.state.trash.remove(index);
                    this.state.status = format!("Deleted {} permanently", entry.file.name);
                }
                this.state.pending_purge = None;
                cx.notify();
            });
        }
        return;
    }

    let op = ops::purge_op(&entry.file.object_id, entry.file.revision);
    spawn_write(
        view,
        vec![op],
        vec![entry.file.object_id.clone()],
        format!("Deleted {} permanently", entry.file.name),
        cx,
    );
}

/// Purge everything in the trash, one operation per object.
///
/// Each entry is its own operation, as it is in the original's retention sweep,
/// so one that fails leaves the rest purged instead of one all-or-nothing
/// request that a single refusal would undo.
fn empty_trash(view: Entity<TDriveView>, cx: &mut App) {
    let (client, entries, demo) = {
        let this = view.read(cx);
        (
            this.state.tele.clone(),
            this.state.trash.clone(),
            this.state.demo,
        )
    };
    if entries.is_empty() {
        return;
    }

    if client.is_none() {
        if demo {
            view.update(cx, |this, cx| {
                let count = this.state.trash.len();
                this.state.trash.clear();
                this.state.confirm_empty_trash = false;
                this.state.pending_purge = None;
                this.state.status = format!("Emptied the trash ({count} items)");
                cx.notify();
            });
        }
        return;
    }

    let ops = entries
        .iter()
        .map(|entry| ops::purge_op(&entry.file.object_id, entry.file.revision))
        .collect();
    let purge = entries
        .iter()
        .map(|entry| entry.file.object_id.clone())
        .collect();
    let count = entries.len();
    spawn_write(
        view,
        ops,
        purge,
        format!("Emptied the trash ({count} items)"),
        cx,
    );
}

fn thumb_path(drive_id: i64, message_id: i32) -> PathBuf {
    thumbnails_dir().join(format!("{drive_id}_{message_id}"))
}

/// Where the gallery keeps the pictures it has fetched.
fn thumbnails_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|dirs| dirs.cache_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tdrive")
        .join("thumbs")
}

/// Throw away every cached thumbnail.
///
/// Called when the vault is unlocked. Anything fetched while it was locked was
/// ciphertext, and leaving it on disk would keep the gallery showing a broken
/// tile for a file the key has since made readable.
fn clear_thumbnail_cache() {
    let _ = std::fs::remove_dir_all(thumbnails_dir());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture of a file is kept small, so the cache holds pictures rather
    /// than a second copy of the drive.
    #[test]
    fn a_thumbnail_is_fitted_into_a_square() {
        let dir = std::env::temp_dir().join("tdrive-rs-thumbnail-test");
        std::fs::create_dir_all(&dir).expect("a place to work in");
        let source = dir.join("source.png");
        let dest = dir.join("thumb");

        let picture = image::RgbImage::from_fn(1000, 600, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        picture.save(&source).expect("the picture to be written");

        write_thumbnail(&source, &dest).expect("the picture to be made");

        // The cache keeps the picture without a name, so the format is read
        // from the bytes rather than from the file's name.
        let made = image::ImageReader::open(&dest)
            .expect("the picture to be there")
            .with_guessed_format()
            .expect("the picture to have a format")
            .decode()
            .expect("the picture to be readable");
        assert_eq!(made.width(), GALLERY_PREVIEW_SIDE);
        assert!(made.height() <= GALLERY_PREVIEW_SIDE);
        // The shape of the picture is kept, so a wide picture stays wide.
        assert!(made.height() < made.width());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that is not a picture cannot be made into one, and the gallery
    /// says so rather than keeping a broken file.
    #[test]
    fn a_file_that_is_not_a_picture_makes_no_thumbnail() {
        let dir = std::env::temp_dir().join("tdrive-rs-thumbnail-test-bad");
        std::fs::create_dir_all(&dir).expect("a place to work in");
        let source = dir.join("source");
        let dest = dir.join("thumb");
        std::fs::write(&source, b"not a picture at all").expect("the file to be written");

        assert!(write_thumbnail(&source, &dest).is_err());
        assert!(!dest.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
