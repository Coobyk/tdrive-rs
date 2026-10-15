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

use std::sync::OnceLock;

use gpui::{Rgba, rgb, rgba};

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
    pub warning: u32,
    pub success: u32,

    // Translucent overlays. Alpha is the last byte.
    pub overlay_neutral_1: u32,
    pub overlay_neutral_2: u32,
    pub overlay_white_1: u32,
    pub overlay_accent_1: u32,
    pub overlay_accent_2: u32,
    pub overlay_danger_1: u32,
    pub overlay_danger_2: u32,
    pub overlay_success_1: u32,
    pub overlay_warning_1: u32,
}

/// Quiet Relay: the original's default dark palette.
const QUIET_RELAY: Palette = Palette {
    canvas: 0x0e171c,
    surface_0: 0x121e24,
    surface_1: 0x18272f,
    surface_2: 0x20343d,
    surface_3: 0x29454f,

    text: 0xe7f0f2,
    text_soft: 0xc1d0d4,
    text_muted: 0x94a7ad,
    text_subtle: 0x899da4,

    border: 0x5a737c,
    border_soft: 0x30464f,

    accent: 0x2aabee,
    accent_hover: 0x4bb8ef,
    accent_strong: 0x168fca,
    on_accent: 0x0e171c,
    danger: 0xde6c76,
    warning: 0xd6a34e,
    success: 0x49b889,

    overlay_neutral_1: 0xe7f0f20a,
    overlay_neutral_2: 0xe7f0f212,
    overlay_white_1: 0xffffff0a,
    overlay_accent_1: 0x2aabee1a,
    overlay_accent_2: 0x2aabee29,
    overlay_danger_1: 0xde6c761a,
    overlay_danger_2: 0xde6c7647,
    overlay_success_1: 0x49b8891a,
    overlay_warning_1: 0xd6a34e1c,
};

/// The alternate dark palette the original ships alongside Quiet Relay.
const TOKYO_NIGHT: Palette = Palette {
    canvas: 0x1a1b26,
    surface_0: 0x16161e,
    surface_1: 0x1f2335,
    surface_2: 0x24283b,
    surface_3: 0x2f3650,

    text: 0xf5f7ff,
    text_soft: 0xc0caf5,
    text_muted: 0x8b95c5,
    text_subtle: 0x848ebb,

    border: 0x2f3650,
    border_soft: 0x292e42,

    accent: 0x7aa2f7,
    accent_hover: 0x8fb4ff,
    accent_strong: 0x5d85e0,
    on_accent: 0x16161e,
    danger: 0xf7768e,
    warning: 0xe0af68,
    success: 0x34c759,

    overlay_neutral_1: 0xffffff0a,
    overlay_neutral_2: 0xffffff14,
    overlay_white_1: 0xffffff0a,
    overlay_accent_1: 0x7aa2f71a,
    overlay_accent_2: 0x7aa2f729,
    overlay_danger_1: 0xf7768e1a,
    overlay_danger_2: 0xf7768e47,
    overlay_success_1: 0x34c7591f,
    overlay_warning_1: 0xe0af681f,
};

fn palette() -> &'static Palette {
    static ACTIVE: OnceLock<Palette> = OnceLock::new();
    ACTIVE.get_or_init(|| match std::env::var("TDRIVE_THEME").as_deref() {
        Ok("tokyo-night") => TOKYO_NIGHT,
        _ => QUIET_RELAY,
    })
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
pub fn overlay_warning_1() -> Rgba {
    rgba(palette().overlay_warning_1)
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
pub const COL_ACTIONS_W: f32 = 100.0;
/// The 12px `column-gap` between the table's cells.
pub const COL_GAP: f32 = 12.0;
/// The 20px `--space-5` a row pads itself with.
pub const ROW_PAD: f32 = 20.0;
/// The extra 10px the table header indents its Name cell by.
pub const NAME_INSET: f32 = 10.0;
