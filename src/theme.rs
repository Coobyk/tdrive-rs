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

use gpui::{Rgba, rgb, rgba};

use tdrive_rs::palettes::Palette;

pub use tdrive_rs::themes::{Appearance, THEMES, Theme, set_theme, theme_by_id};

fn palette() -> &'static Palette {
    tdrive_rs::themes::active_palette()
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
