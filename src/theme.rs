//! Color themes. Add a new palette by appending a `Theme` literal and
//! pushing it into `ALL`.

use ratatui::style::Color;

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub name: &'static str,
    pub bg: Color,
    pub fg: Color,
    pub surface: Color,
    pub border: Color,
    pub border_focused: Color,
    pub accent: Color,
    pub accent_fg: Color,
    pub muted: Color,
    pub ok: Color,
    pub warn: Color,
    pub err: Color,
    pub title: Color,
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb(
        ((hex >> 16) & 0xff) as u8,
        ((hex >> 8) & 0xff) as u8,
        (hex & 0xff) as u8,
    )
}

pub const DEFAULT: Theme = Theme {
    name: "default",
    bg: Color::Reset,
    fg: Color::Reset,
    surface: Color::Reset,
    border: Color::DarkGray,
    border_focused: Color::Cyan,
    accent: Color::Cyan,
    accent_fg: Color::Black,
    muted: Color::DarkGray,
    ok: Color::Green,
    warn: Color::Yellow,
    err: Color::Red,
    title: Color::Cyan,
};

pub const CATPPUCCIN_LATTE: Theme = Theme {
    name: "catppuccin-latte",
    bg: rgb(0xeff1f5),
    fg: rgb(0x4c4f69),
    surface: rgb(0xccd0da),
    border: rgb(0x9ca0b0),
    border_focused: rgb(0x8839ef),
    accent: rgb(0x8839ef),
    accent_fg: rgb(0xeff1f5),
    muted: rgb(0x6c6f85),
    ok: rgb(0x40a02b),
    warn: rgb(0xdf8e1d),
    err: rgb(0xd20f39),
    title: rgb(0x1e66f5),
};

pub const CATPPUCCIN_FRAPPE: Theme = Theme {
    name: "catppuccin-frappe",
    bg: rgb(0x303446),
    fg: rgb(0xc6d0f5),
    surface: rgb(0x414559),
    border: rgb(0x737994),
    border_focused: rgb(0xca9ee6),
    accent: rgb(0xca9ee6),
    accent_fg: rgb(0x303446),
    muted: rgb(0x838ba7),
    ok: rgb(0xa6d189),
    warn: rgb(0xe5c890),
    err: rgb(0xe78284),
    title: rgb(0x8caaee),
};

pub const CATPPUCCIN_MACCHIATO: Theme = Theme {
    name: "catppuccin-macchiato",
    bg: rgb(0x24273a),
    fg: rgb(0xcad3f5),
    surface: rgb(0x363a4f),
    border: rgb(0x6e738d),
    border_focused: rgb(0xc6a0f6),
    accent: rgb(0xc6a0f6),
    accent_fg: rgb(0x24273a),
    muted: rgb(0x8087a2),
    ok: rgb(0xa6da95),
    warn: rgb(0xeed49f),
    err: rgb(0xed8796),
    title: rgb(0x8aadf4),
};

pub const CATPPUCCIN_MOCHA: Theme = Theme {
    name: "catppuccin-mocha",
    bg: rgb(0x1e1e2e),
    fg: rgb(0xcdd6f4),
    surface: rgb(0x313244),
    border: rgb(0x6c7086),
    border_focused: rgb(0xcba6f7),
    accent: rgb(0xcba6f7),
    accent_fg: rgb(0x1e1e2e),
    muted: rgb(0x7f849c),
    ok: rgb(0xa6e3a1),
    warn: rgb(0xf9e2af),
    err: rgb(0xf38ba8),
    title: rgb(0x89b4fa),
};

pub const ROSE_PINE: Theme = Theme {
    name: "rose-pine",
    bg: rgb(0x191724),
    fg: rgb(0xe0def4),
    surface: rgb(0x1f1d2e),
    border: rgb(0x403d52),
    border_focused: rgb(0xc4a7e7),
    accent: rgb(0xc4a7e7),
    accent_fg: rgb(0x191724),
    muted: rgb(0x6e6a86),
    ok: rgb(0x9ccfd8),
    warn: rgb(0xf6c177),
    err: rgb(0xeb6f92),
    title: rgb(0xebbcba),
};

pub const ROSE_PINE_MOON: Theme = Theme {
    name: "rose-pine-moon",
    bg: rgb(0x232136),
    fg: rgb(0xe0def4),
    surface: rgb(0x2a273f),
    border: rgb(0x44415a),
    border_focused: rgb(0xc4a7e7),
    accent: rgb(0xc4a7e7),
    accent_fg: rgb(0x232136),
    muted: rgb(0x6e6a86),
    ok: rgb(0x9ccfd8),
    warn: rgb(0xf6c177),
    err: rgb(0xeb6f92),
    title: rgb(0xea9a97),
};

pub const ROSE_PINE_DAWN: Theme = Theme {
    name: "rose-pine-dawn",
    bg: rgb(0xfaf4ed),
    fg: rgb(0x575279),
    surface: rgb(0xfffaf3),
    border: rgb(0xcecacd),
    border_focused: rgb(0x907aa9),
    accent: rgb(0x907aa9),
    accent_fg: rgb(0xfaf4ed),
    muted: rgb(0x797593),
    ok: rgb(0x286983),
    warn: rgb(0xea9d34),
    err: rgb(0xb4637a),
    title: rgb(0xd7827e),
};

pub const TOKYO_NIGHT: Theme = Theme {
    name: "tokyo-night",
    bg: rgb(0x1a1b26),
    fg: rgb(0xc0caf5),
    surface: rgb(0x24283b),
    border: rgb(0x414868),
    border_focused: rgb(0x7aa2f7),
    accent: rgb(0x7aa2f7),
    accent_fg: rgb(0x1a1b26),
    muted: rgb(0x565f89),
    ok: rgb(0x9ece6a),
    warn: rgb(0xe0af68),
    err: rgb(0xf7768e),
    title: rgb(0xbb9af7),
};

pub const TOKYO_NIGHT_STORM: Theme = Theme {
    name: "tokyo-night-storm",
    bg: rgb(0x24283b),
    fg: rgb(0xc0caf5),
    surface: rgb(0x2f334d),
    border: rgb(0x545c7e),
    border_focused: rgb(0x7aa2f7),
    accent: rgb(0x7aa2f7),
    accent_fg: rgb(0x24283b),
    muted: rgb(0x565f89),
    ok: rgb(0x9ece6a),
    warn: rgb(0xe0af68),
    err: rgb(0xf7768e),
    title: rgb(0xbb9af7),
};

pub const GRUVBOX_DARK: Theme = Theme {
    name: "gruvbox-dark",
    bg: rgb(0x282828),
    fg: rgb(0xebdbb2),
    surface: rgb(0x3c3836),
    border: rgb(0x665c54),
    border_focused: rgb(0xfabd2f),
    accent: rgb(0xfabd2f),
    accent_fg: rgb(0x282828),
    muted: rgb(0x928374),
    ok: rgb(0xb8bb26),
    warn: rgb(0xfe8019),
    err: rgb(0xfb4934),
    title: rgb(0x8ec07c),
};

pub const GRUVBOX_LIGHT: Theme = Theme {
    name: "gruvbox-light",
    bg: rgb(0xfbf1c7),
    fg: rgb(0x3c3836),
    surface: rgb(0xebdbb2),
    border: rgb(0xa89984),
    border_focused: rgb(0xb57614),
    accent: rgb(0xb57614),
    accent_fg: rgb(0xfbf1c7),
    muted: rgb(0x7c6f64),
    ok: rgb(0x79740e),
    warn: rgb(0xaf3a03),
    err: rgb(0x9d0006),
    title: rgb(0x427b58),
};

pub const NORD: Theme = Theme {
    name: "nord",
    bg: rgb(0x2e3440),
    fg: rgb(0xeceff4),
    surface: rgb(0x3b4252),
    border: rgb(0x4c566a),
    border_focused: rgb(0x88c0d0),
    accent: rgb(0x88c0d0),
    accent_fg: rgb(0x2e3440),
    muted: rgb(0x4c566a),
    ok: rgb(0xa3be8c),
    warn: rgb(0xebcb8b),
    err: rgb(0xbf616a),
    title: rgb(0x81a1c1),
};

pub const DRACULA: Theme = Theme {
    name: "dracula",
    bg: rgb(0x282a36),
    fg: rgb(0xf8f8f2),
    surface: rgb(0x44475a),
    border: rgb(0x6272a4),
    border_focused: rgb(0xbd93f9),
    accent: rgb(0xbd93f9),
    accent_fg: rgb(0x282a36),
    muted: rgb(0x6272a4),
    ok: rgb(0x50fa7b),
    warn: rgb(0xf1fa8c),
    err: rgb(0xff5555),
    title: rgb(0xff79c6),
};

pub const ALL: &[Theme] = &[
    DEFAULT,
    CATPPUCCIN_LATTE,
    CATPPUCCIN_FRAPPE,
    CATPPUCCIN_MACCHIATO,
    CATPPUCCIN_MOCHA,
    ROSE_PINE,
    ROSE_PINE_MOON,
    ROSE_PINE_DAWN,
    TOKYO_NIGHT,
    TOKYO_NIGHT_STORM,
    GRUVBOX_DARK,
    GRUVBOX_LIGHT,
    NORD,
    DRACULA,
];

pub fn by_name(name: &str) -> Option<Theme> {
    ALL.iter()
        .copied()
        .find(|t| t.name.eq_ignore_ascii_case(name))
}

pub fn index_of(name: &str) -> usize {
    ALL.iter().position(|t| t.name == name).unwrap_or(0)
}
