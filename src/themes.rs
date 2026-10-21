//! The theme catalogue: the original's palettes, and the one on screen.
//!
//! The colours themselves live in [`crate::palettes`]; this is the list of
//! themes the original ships, the name each one is shown under, and whether
//! it is a light one or a dark one. The phone app uses the same list, so the
//! two applications cannot drift apart on what a theme is.

use std::sync::{OnceLock, RwLock};

use crate::palettes::{self, Palette};

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
    Theme {
        id: "daybreak",
        name: "Daybreak",
        appearance: Appearance::Light,
        preview: [0xeef4f5, 0xe6eef0, 0x1689be, 0x17262c],
    },
    Theme {
        id: "porcelain",
        name: "Porcelain",
        appearance: Appearance::Light,
        preview: [0xf3f5f9, 0xeceff5, 0x315fc4, 0x101828],
    },
    Theme {
        id: "catppuccin-latte",
        name: "Catppuccin Latte",
        appearance: Appearance::Light,
        preview: [0xeff1f5, 0xdfe3eb, 0x1c60e8, 0x4c4f69],
    },
    Theme {
        id: "solarized-light",
        name: "Solarized Light",
        appearance: Appearance::Light,
        preview: [0xfdf6e3, 0xeee8d5, 0x126d9f, 0x073642],
    },
    Theme {
        id: "gruvbox-light",
        name: "Gruvbox Light",
        appearance: Appearance::Light,
        preview: [0xfbf1c7, 0xebdbb2, 0x076678, 0x282828],
    },
    Theme {
        id: "rose-pine-dawn",
        name: "Rosé Pine Dawn",
        appearance: Appearance::Light,
        preview: [0xfaf4ed, 0xf2e9e1, 0x7a5f95, 0x575279],
    },
    Theme {
        id: "everforest-light",
        name: "Everforest Light",
        appearance: Appearance::Light,
        preview: [0xfffbef, 0xf2efdf, 0x4c7a1a, 0x5c6a72],
    },
    Theme {
        id: "quiet-relay",
        name: "Quiet Relay",
        appearance: Appearance::Dark,
        preview: [0x0e171c, 0x20343d, 0x2aabee, 0xe7f0f2],
    },
    Theme {
        id: "tokyo-night",
        name: "Tokyo Night",
        appearance: Appearance::Dark,
        preview: [0x1a1b26, 0x24283b, 0x7aa2f7, 0xf5f7ff],
    },
    Theme {
        id: "catppuccin-mocha",
        name: "Catppuccin Mocha",
        appearance: Appearance::Dark,
        preview: [0x1e1e2e, 0x313244, 0x89b4fa, 0xf1f3fb],
    },
    Theme {
        id: "dracula",
        name: "Dracula",
        appearance: Appearance::Dark,
        preview: [0x282a36, 0x383a4a, 0xbd93f9, 0xf8f8f2],
    },
    Theme {
        id: "solarized-dark",
        name: "Solarized Dark",
        appearance: Appearance::Dark,
        preview: [0x002b36, 0x0d414d, 0x36a4d9, 0xf5efdd],
    },
    Theme {
        id: "gruvbox-dark",
        name: "Gruvbox Dark",
        appearance: Appearance::Dark,
        preview: [0x282828, 0x3c3836, 0x83a598, 0xfbf1c7],
    },
    Theme {
        id: "nord",
        name: "Nord",
        appearance: Appearance::Dark,
        preview: [0x2e3440, 0x3b4252, 0x88c0d0, 0xeceff4],
    },
    Theme {
        id: "everforest-dark",
        name: "Everforest Dark",
        appearance: Appearance::Dark,
        preview: [0x272e33, 0x3d484d, 0xa7c080, 0xd3c6aa],
    },
    Theme {
        id: "ayu-dark",
        name: "Ayu Dark",
        appearance: Appearance::Dark,
        preview: [0x0b0e14, 0x1a1f2a, 0xe6b450, 0xbfbdb6],
    },
    Theme {
        id: "rose-pine",
        name: "Rosé Pine",
        appearance: Appearance::Dark,
        preview: [0x191724, 0x26233a, 0xc4a7e7, 0xe0def4],
    },
];

/// The theme the original starts in: Quiet Relay, which is
/// `DEFAULT_THEME_PREFERENCE.darkThemeId` there.
pub const DEFAULT_THEME: &str = "quiet-relay";

/// A theme by its name, if the catalogue has it.
pub fn theme_by_id(id: &str) -> Option<&'static Theme> {
    THEMES.iter().find(|theme| theme.id == id)
}

/// The palette of a theme by its name.
pub fn palette_of(id: &str) -> Option<&'static Palette> {
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
pub fn ensure_started() {
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

/// The palette on screen.
pub fn active_palette() -> &'static Palette {
    ensure_started();
    *ACTIVE.read().unwrap()
}
