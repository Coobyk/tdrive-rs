//! TDrive visual tokens.
//!
//! Mirrors the palette contract from the Svelte frontend
//! (`frontend/src/ui/theme/theme-palettes.css`) plus the spacing and type scale
//! from `frontend/src/styles/tokens.css`. The default palette is Quiet Relay,
//! which is `DEFAULT_THEME_PREFERENCE.darkThemeId` in the original; set
//! `TDRIVE_THEME=tokyo-night` to use the alternate dark palette.
//!
//! Every name here maps onto one CSS custom property the original consumes, so
//! a rule that says `color: var(--text-muted)` becomes `theme::text_muted()`.
//!
//! The palette is a token catalogue: it carries the whole contract the
//! stylesheets use, so a few entries have no caller yet.

#![allow(dead_code)]

use std::sync::{OnceLock, RwLock};

use gpui::{Rgba, rgb, rgba};

use crate::palettes;

/// One palette's worth of semantic colours. Values are `0xRRGGBB` unless the
/// field is an overlay, which is `0xRRGGBBAA`.
pub struct Palette {
    pub canvas: u32,
    pub surface_0: u32,
    pub surface_1: u32,
    pub surface_2: u32,
    pub surface_3: u32,

    pub text: u32,
    pub text_soft: u32,
    pub text_muted: u32,
    pub text_subtle: u32,

    pub border: u32,
    pub border_soft: u32,

    pub accent: u32,
    pub accent_hover: u32,
    pub accent_strong: u32,
    pub on_accent: u32,
    pub danger: u32,
    pub on_danger: u32,
    pub warning: u32,
    pub success: u32,

    // Translucent overlays. Alpha is the last byte.
    pub overlay_neutral_1: u32,
    pub overlay_neutral_2: u32,
    pub overlay_neutral_3: u32,
    pub overlay_white_1: u32,
    pub overlay_white_2: u32,
    pub overlay_white_3: u32,
    pub overlay_accent_1: u32,
    pub overlay_accent_2: u32,
    pub overlay_accent_3: u32,
    pub overlay_danger_1: u32,
    pub overlay_danger_2: u32,
    pub overlay_success_1: u32,
    pub overlay_success_2: u32,
    pub overlay_warning_1: u32,
    pub overlay_warning_2: u32,

    // The glass the original's menus and popovers are made of, and the scrim
    // that dims the shell behind a modal.
    pub glass_bg: u32,
    pub glass_border: u32,
    pub glass_highlight: u32,
    pub scrim: u32,
}

/// One theme in the original's catalogue.
pub struct Theme {
    /// The name the original's `data-theme` attribute carries.
    pub id: &'static str,
    /// The name the original shows for it.
    pub name: &'static str,
    /// Whether the theme is a light one or a dark one.
    pub appearance: Appearance,
    /// The four colours the original's palette card draws: the canvas, the
    /// surface, the accent and the text.
    pub preview: [u32; 4],
}

/// Whether a theme is a light one or a dark one.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
}

/// The catalogue, in the original's order: the light themes first, then the
/// dark ones.
pub const THEMES: &[Theme] = &[
    Theme { id: "daybreak", name: "Daybreak", appearance: Appearance::Light, preview: [0xeef4f5, 0xe6eef0, 0x1689be, 0x17262c] },
    Theme { id: "porcelain", name: "Porcelain", appearance: Appearance::Light, preview: [0xf3f5f9, 0xeceff5, 0x315fc4, 0x101828] },
    Theme { id: "catppuccin-latte", name: "Catppuccin Latte", appearance: Appearance::Light, preview: [0xeff1f5, 0xdfe3eb, 0x1c60e8, 0x4c4f69] },
    Theme { id: "solarized-light", name: "Solarized Light", appearance: Appearance::Light, preview: [0xfdf6e3, 0xeee8d5, 0x126d9f, 0x073642] },
    Theme { id: "gruvbox-light", name: "Gruvbox Light", appearance: Appearance::Light, preview: [0xfbf1c7, 0xebdbb2, 0x076678, 0x282828] },
    Theme { id: "rose-pine-dawn", name: "Rosé Pine Dawn", appearance: Appearance::Light, preview: [0xfaf4ed, 0xf2e9e1, 0x7a5f95, 0x575279] },
    Theme { id: "everforest-light", name: "Everforest Light", appearance: Appearance::Light, preview: [0xfffbef, 0xf2efdf, 0x4c7a1a, 0x5c6a72] },
    Theme { id: "quiet-relay", name: "Quiet Relay", appearance: Appearance::Dark, preview: [0x0e171c, 0x20343d, 0x2aabee, 0xe7f0f2] },
    Theme { id: "tokyo-night", name: "Tokyo Night", appearance: Appearance::Dark, preview: [0x1a1b26, 0x24283b, 0x7aa2f7, 0xf5f7ff] },
    Theme { id: "catppuccin-mocha", name: "Catppuccin Mocha", appearance: Appearance::Dark, preview: [0x1e1e2e, 0x313244, 0x89b4fa, 0xf1f3fb] },
    Theme { id: "dracula", name: "Dracula", appearance: Appearance::Dark, preview: [0x282a36, 0x383a4a, 0xbd93f9, 0xf8f8f2] },
    Theme { id: "solarized-dark", name: "Solarized Dark", appearance: Appearance::Dark, preview: [0x002b36, 0x0d414d, 0x36a4d9, 0xf5efdd] },
    Theme { id: "gruvbox-dark", name: "Gruvbox Dark", appearance: Appearance::Dark, preview: [0x282828, 0x3c3836, 0x83a598, 0xfbf1c7] },
    Theme { id: "nord", name: "Nord", appearance: Appearance::Dark, preview: [0x2e3440, 0x3b4252, 0x88c0d0, 0xeceff4] },
    Theme { id: "everforest-dark", name: "Everforest Dark", appearance: Appearance::Dark, preview: [0x272e33, 0x3d484d, 0xa7c080, 0xd3c6aa] },
    Theme { id: "ayu-dark", name: "Ayu Dark", appearance: Appearance::Dark, preview: [0x0b0e14, 0x1a1f2a, 0xe6b450, 0xbfbdb6] },
    Theme { id: "rose-pine", name: "Rosé Pine", appearance: Appearance::Dark, preview: [0x191724, 0x26233a, 0xc4a7e7, 0xe0def4] },
];

/// The theme the original starts in: Quiet Relay, which is
/// `DEFAULT_THEME_PREFERENCE.darkThemeId` there.
pub const DEFAULT_THEME: &str = "quiet-relay";

/// A theme by its name, if the catalogue has it.
pub fn theme_by_id(id: &str) -> Option<&'static Theme> {
    THEMES.iter().find(|theme| theme.id == id)
}

/// The palette of a theme by its name.
fn palette_of(id: &str) -> Option<&'static Palette> {
    Some(match id {
        "daybreak" => &palettes::DAYBREAK,
        "porcelain" => &palettes::PORCELAIN,
        "catppuccin-latte" => &palettes::CATPPUCCIN_LATTE,
        "solarized-light" => &palettes::SOLARIZED_LIGHT,
        "gruvbox-light" => &palettes::GRUVBOX_LIGHT,
        "rose-pine-dawn" => &palettes::ROSE_PINE_DAWN,
        "everforest-light" => &palettes::EVERFOREST_LIGHT,
        "quiet-relay" => &palettes::QUIET_RELAY,
        "tokyo-night" => &palettes::TOKYO_NIGHT,
        "catppuccin-mocha" => &palettes::CATPPUCCIN_MOCHA,
        "dracula" => &palettes::DRACULA,
        "solarized-dark" => &palettes::SOLARIZED_DARK,
        "gruvbox-dark" => &palettes::GRUVBOX_DARK,
        "nord" => &palettes::NORD,
        "everforest-dark" => &palettes::EVERFOREST_DARK,
        "ayu-dark" => &palettes::AYU_DARK,
        "rose-pine" => &palettes::ROSE_PINE,
        _ => return None,
    })
}

/// The palette on screen.
static ACTIVE: RwLock<&'static Palette> = RwLock::new(&palettes::QUIET_RELAY);
/// The name of the theme on screen, beside its palette.
static ACTIVE_ID: RwLock<String> = RwLock::new(String::new());

/// Put a theme on screen.
///
/// A name the catalogue does not have is refused, the way the original's
/// closed catalogue refuses one: the palette is data, and data that names a
/// theme nobody defined is not a theme.
pub fn set_theme(id: &str) -> bool {
    // The start-up runs first, so a theme chosen before the first draw is not
    // undone by it.
    ensure_started();
    let Some(palette) = palette_of(id) else {
        return false;
    };
    *ACTIVE_ID.write().unwrap() = id.to_string();
    *ACTIVE.write().unwrap() = palette;
    true
}

/// The name of the theme in use.
pub fn active_id() -> String {
    ensure_started();
    ACTIVE_ID.read().unwrap().clone()
}

/// Start on the theme the environment asks for, or on the original's own
/// default.
fn ensure_started() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let asked = std::env::var("TDRIVE_THEME")
            .ok()
            .and_then(|id| theme_by_id(&id).map(|theme| theme.id.to_string()));
        let id = asked.unwrap_or_else(|| DEFAULT_THEME.to_string());
        *ACTIVE_ID.write().unwrap() = id.clone();
        *ACTIVE.write().unwrap() = palette_of(&id).unwrap_or(&palettes::QUIET_RELAY);
    });
}

fn palette() -> &'static Palette {
    ensure_started();
    *ACTIVE.read().unwrap()
}

// --- Surfaces -------------------------------------------------------------

pub fn canvas() -> Rgba {
    rgb(palette().canvas)
}
pub fn surface_0() -> Rgba {
    rgb(palette().surface_0)
}
pub fn surface_1() -> Rgba {
    rgb(palette().surface_1)
}
pub fn surface_2() -> Rgba {
    rgb(palette().surface_2)
}
pub fn surface_3() -> Rgba {
    rgb(palette().surface_3)
}
/// `--surface-control`, the fill a control sits on.
pub fn surface_control() -> Rgba {
    surface_1()
}
/// `--bg-panel`, the raised surface a hover paints.
pub fn panel() -> Rgba {
    surface_2()
}
/// The breadcrumb bar: `color-mix(in srgb, surface-0 58%, transparent)`.
pub fn breadcrumb_bg() -> Rgba {
    let s = palette().surface_0;
    rgba((s << 8) | 0x94)
}

// --- Text -----------------------------------------------------------------

pub fn text() -> Rgba {
    rgb(palette().text)
}
/// `--text-main`, which the palette points at `--color-text-soft`.
pub fn text_soft() -> Rgba {
    rgb(palette().text_soft)
}
pub fn text_muted() -> Rgba {
    rgb(palette().text_muted)
}
pub fn text_subtle() -> Rgba {
    rgb(palette().text_subtle)
}

// --- Lines ----------------------------------------------------------------

/// `--color-border`.
pub fn border() -> Rgba {
    rgb(palette().border)
}
/// `--border`, which the token sheet aliases to `--color-border-soft`. This is
/// the line the shell actually draws between regions and around controls.
pub fn border_soft() -> Rgba {
    rgb(palette().border_soft)
}

// --- Accents and status ---------------------------------------------------

pub fn accent() -> Rgba {
    rgb(palette().accent)
}
pub fn accent_hover() -> Rgba {
    rgb(palette().accent_hover)
}
pub fn accent_strong() -> Rgba {
    rgb(palette().accent_strong)
}
pub fn on_accent() -> Rgba {
    rgb(palette().on_accent)
}
pub fn danger() -> Rgba {
    rgb(palette().danger)
}
pub fn warning() -> Rgba {
    rgb(palette().warning)
}
pub fn success() -> Rgba {
    rgb(palette().success)
}

// --- Translucent overlays -------------------------------------------------

/// `--overlay-neutral-1`.
pub fn overlay_1() -> Rgba {
    rgba(palette().overlay_neutral_1)
}
/// `--overlay-neutral-2`, the row hover.
pub fn overlay_2() -> Rgba {
    rgba(palette().overlay_neutral_2)
}
/// `--overlay-white-1`.
pub fn overlay_white_1() -> Rgba {
    rgba(palette().overlay_white_1)
}
pub fn overlay_accent_1() -> Rgba {
    rgba(palette().overlay_accent_1)
}
pub fn overlay_accent_2() -> Rgba {
    rgba(palette().overlay_accent_2)
}
pub fn overlay_danger_1() -> Rgba {
    rgba(palette().overlay_danger_1)
}
pub fn overlay_danger_2() -> Rgba {
    rgba(palette().overlay_danger_2)
}
pub fn overlay_success_1() -> Rgba {
    rgba(palette().overlay_success_1)
}
pub fn overlay_success_2() -> Rgba {
    rgba(palette().overlay_success_2)
}
pub fn overlay_warning_1() -> Rgba {
    rgba(palette().overlay_warning_1)
}
pub fn overlay_warning_2() -> Rgba {
    rgba(palette().overlay_warning_2)
}
pub fn overlay_neutral_3() -> Rgba {
    rgba(palette().overlay_neutral_3)
}
pub fn overlay_white_2() -> Rgba {
    rgba(palette().overlay_white_2)
}
pub fn overlay_white_3() -> Rgba {
    rgba(palette().overlay_white_3)
}
pub fn overlay_accent_3() -> Rgba {
    rgba(palette().overlay_accent_3)
}
pub fn on_danger() -> Rgba {
    rgb(palette().on_danger)
}

/// `--glass-bg`, the fill a menu or a popover is made of.
///
/// The original keeps this one translucent, so the alpha matters: a theme
/// whose glass is a plain colour is an opaque one.
pub fn glass_bg() -> Rgba {
    rgba(palette().glass_bg)
}
/// `--glass-border`.
pub fn glass_border() -> Rgba {
    rgba(palette().glass_border)
}
/// `--glass-highlight`.
pub fn glass_highlight() -> Rgba {
    rgba(palette().glass_highlight)
}
/// `--scrim`, the dimming the shell gets behind a modal.
pub fn scrim() -> Rgba {
    rgba(palette().scrim)
}

// --- Type scale -----------------------------------------------------------
// `--type-*` in pixels at the default root size sitting at 16px.

pub const TYPE_XS: f32 = 12.0;
pub const TYPE_SM: f32 = 13.0;
pub const TYPE_BASE: f32 = 14.0;
pub const TYPE_MD: f32 = 16.0;
pub const TYPE_LG: f32 = 17.0;
pub const TYPE_XL: f32 = 20.0;

// --- Metrics --------------------------------------------------------------
// The fixed lengths the shell draws with, from `chrome.css` and `files.css`.

pub const SIDEBAR_W: f32 = 240.0;
pub const SIDEBAR_PAD: f32 = 20.0;
pub const LOGO_MB: f32 = 40.0;
pub const SEARCH_W: f32 = 300.0;
pub const HEADER_H: f32 = 56.0;
pub const BREADCRUMB_H: f32 = 44.0;
pub const TABLE_HEADER_H: f32 = 32.0;
/// `--touch-target`.
pub const ROW_H: f32 = 44.0;
/// `--control-md`, the row's leading tile.
pub const TILE: f32 = 32.0;
/// `--control-sm`, the row's quiet icon buttons.
pub const ACTION: f32 = 28.0;
pub const COL_DATE_W: f32 = 150.0;
pub const COL_SIZE_W: f32 = 120.0;
pub const COL_ACTIONS_W: f32 = 140.0;
/// The 12px `column-gap` between the table's cells.
pub const COL_GAP: f32 = 12.0;
/// The 20px `--space-5` a row pads itself with.
pub const ROW_PAD: f32 = 20.0;
/// The extra 10px the table header indents its Name cell by.
pub const NAME_INSET: f32 = 10.0;
