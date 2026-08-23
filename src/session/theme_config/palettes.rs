//! The thirty-six built-in palettes, as data.
//!
//! Split from `theme_config`'s behaviour so [`super::ThemePreset::palette`]'s
//! dispatch is the one visible seam: everything in this file is a colour
//! table, and adding a preset means adding one function here plus its arm
//! there. The enumeration order users see lives with the dispatch, not here.

use ratatui::style::Color;

use super::{PaletteSlots, ThemePalette};

pub(super) fn catppuccin_mocha_palette() -> ThemePalette {
    let mauve = Color::Rgb(0xCB, 0xA6, 0xF7);
    let pink = Color::Rgb(0xF5, 0xC2, 0xE7);
    let green = Color::Rgb(0xA6, 0xE3, 0xA1);
    let yellow = Color::Rgb(0xF9, 0xE2, 0xAF);
    let red = Color::Rgb(0xF3, 0x8B, 0xA8);
    let blue = Color::Rgb(0x89, 0xB4, 0xFA);
    let teal = Color::Rgb(0x94, 0xE2, 0xD5);
    let text = Color::Rgb(0xCD, 0xD6, 0xF4);
    let subtext = Color::Rgb(0xA6, 0xAD, 0xC8);
    let overlay = Color::Rgb(0x6C, 0x70, 0x86);
    let surface0 = Color::Rgb(0x31, 0x32, 0x44);
    let surface1 = Color::Rgb(0x45, 0x47, 0x5A);
    let base = Color::Rgb(0x1E, 0x1E, 0x2E);
    PaletteSlots {
        accent: mauve,
        accent_bright: pink,
        green,
        yellow,
        red,
        blue,
        text_primary: text,
        text_secondary: subtext,
        text_muted: overlay,
        border_unfocused: surface1,
        role_name: pink,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: surface1,
        selection_fg: text,
        modal_dim_bg: base,
        modal_bg: surface0,
        modal_border: teal,
        base_bg: base,
    }
    .build()
}

pub(super) fn tokyo_night_palette() -> ThemePalette {
    let blue = Color::Rgb(0x7A, 0xA2, 0xF7);
    let cyan = Color::Rgb(0x7D, 0xCF, 0xFF);
    let purple = Color::Rgb(0xBB, 0x9A, 0xF7);
    let green = Color::Rgb(0x9E, 0xCE, 0x6A);
    let yellow = Color::Rgb(0xE0, 0xAF, 0x68);
    let red = Color::Rgb(0xF7, 0x76, 0x8E);
    let magenta = Color::Rgb(0xC0, 0xCA, 0xF5);
    let text = Color::Rgb(0xC0, 0xCA, 0xF5);
    let subtext = Color::Rgb(0x9A, 0xA5, 0xCE);
    let muted = Color::Rgb(0x56, 0x5F, 0x89);
    let bg = Color::Rgb(0x1A, 0x1B, 0x26);
    let bg_dark = Color::Rgb(0x16, 0x16, 0x1E);
    let bg_highlight = Color::Rgb(0x29, 0x2E, 0x42);
    PaletteSlots {
        accent: blue,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue,
        text_primary: text,
        text_secondary: subtext,
        text_muted: muted,
        border_unfocused: bg_highlight,
        role_name: purple,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: yellow,
        selection_bg: bg_highlight,
        selection_fg: magenta,
        modal_dim_bg: bg_dark,
        modal_bg: bg,
        modal_border: blue,
        base_bg: bg,
    }
    .build()
}

pub(super) fn gruvbox_dark_palette() -> ThemePalette {
    let yellow = Color::Rgb(0xFA, 0xBD, 0x2F);
    let orange = Color::Rgb(0xFE, 0x80, 0x19);
    let red = Color::Rgb(0xFB, 0x49, 0x34);
    let green = Color::Rgb(0xB8, 0xBB, 0x26);
    let aqua = Color::Rgb(0x8E, 0xC0, 0x7C);
    let blue = Color::Rgb(0x83, 0xA5, 0x98);
    let purple = Color::Rgb(0xD3, 0x86, 0x9B);
    let fg = Color::Rgb(0xEB, 0xDB, 0xB2);
    let fg2 = Color::Rgb(0xD5, 0xC4, 0xA1);
    let gray = Color::Rgb(0x92, 0x83, 0x74);
    let bg0 = Color::Rgb(0x28, 0x28, 0x28);
    let bg1 = Color::Rgb(0x3C, 0x38, 0x36);
    let bg_hard = Color::Rgb(0x1D, 0x20, 0x21);
    PaletteSlots {
        accent: yellow,
        accent_bright: orange,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg2,
        text_muted: gray,
        border_unfocused: bg1,
        role_name: purple,
        branch_name: aqua,
        search_bar: blue,
        keybind_hint: orange,
        selection_bg: bg1,
        selection_fg: fg,
        modal_dim_bg: bg_hard,
        modal_bg: bg0,
        modal_border: yellow,
        base_bg: bg0,
    }
    .build()
}

pub(super) fn catppuccin_latte_palette() -> ThemePalette {
    let mauve = Color::Rgb(0x88, 0x39, 0xEF);
    let pink = Color::Rgb(0xEA, 0x76, 0xCB);
    let green = Color::Rgb(0x40, 0xA0, 0x2B);
    let yellow = Color::Rgb(0xDF, 0x8E, 0x1D);
    let red = Color::Rgb(0xD2, 0x0F, 0x39);
    let blue = Color::Rgb(0x1E, 0x66, 0xF5);
    let teal = Color::Rgb(0x17, 0x92, 0x99);
    let text = Color::Rgb(0x4C, 0x4F, 0x69);
    let subtext = Color::Rgb(0x6C, 0x6F, 0x85);
    let overlay = Color::Rgb(0x9C, 0xA0, 0xB0);
    let surface0 = Color::Rgb(0xCC, 0xD0, 0xDA);
    let surface1 = Color::Rgb(0xBC, 0xC0, 0xCC);
    let base = Color::Rgb(0xEF, 0xF1, 0xF5);
    let crust = Color::Rgb(0xDC, 0xE0, 0xE8);
    PaletteSlots {
        accent: mauve,
        accent_bright: pink,
        green,
        yellow,
        red,
        blue,
        text_primary: text,
        text_secondary: subtext,
        text_muted: overlay,
        border_unfocused: surface1,
        role_name: pink,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: surface0,
        selection_fg: text,
        modal_dim_bg: crust,
        modal_bg: base,
        modal_border: teal,
        base_bg: base,
    }
    .build()
}

pub(super) fn tokyo_night_day_palette() -> ThemePalette {
    let blue = Color::Rgb(0x2E, 0x7D, 0xE9);
    let cyan = Color::Rgb(0x00, 0x71, 0x97);
    let purple = Color::Rgb(0x98, 0x54, 0xF1);
    // Deepened from #7847BD so the selected-session name (which now uses
    // `selection_fg`) clears ~4.5:1 contrast on the light `selection_bg` band.
    let magenta = Color::Rgb(0x5A, 0x3A, 0x9E);
    let green = Color::Rgb(0x58, 0x75, 0x39);
    let yellow = Color::Rgb(0x8C, 0x6C, 0x3E);
    let orange = Color::Rgb(0xB1, 0x5C, 0x00);
    let red = Color::Rgb(0xF5, 0x2A, 0x65);
    let text = Color::Rgb(0x37, 0x60, 0xBF);
    let subtext = Color::Rgb(0x6A, 0x6E, 0x8A);
    let muted = Color::Rgb(0x84, 0x8C, 0xB5);
    let bg = Color::Rgb(0xE1, 0xE2, 0xE7);
    let bg_dark = Color::Rgb(0xD0, 0xD5, 0xE1);
    let bg_highlight = Color::Rgb(0xC4, 0xC8, 0xDA);
    PaletteSlots {
        accent: blue,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue,
        text_primary: text,
        text_secondary: subtext,
        text_muted: muted,
        border_unfocused: bg_highlight,
        role_name: purple,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: orange,
        selection_bg: bg_highlight,
        selection_fg: magenta,
        modal_dim_bg: bg_dark,
        modal_bg: bg,
        modal_border: blue,
        base_bg: bg,
    }
    .build()
}

pub(super) fn gruvbox_light_palette() -> ThemePalette {
    let yellow = Color::Rgb(0xB5, 0x76, 0x14);
    let orange = Color::Rgb(0xAF, 0x3A, 0x03);
    let red = Color::Rgb(0x9D, 0x00, 0x06);
    let green = Color::Rgb(0x79, 0x74, 0x0E);
    let aqua = Color::Rgb(0x42, 0x7B, 0x58);
    let blue = Color::Rgb(0x07, 0x66, 0x78);
    let purple = Color::Rgb(0x8F, 0x3F, 0x71);
    let fg = Color::Rgb(0x3C, 0x38, 0x36);
    let fg2 = Color::Rgb(0x50, 0x49, 0x45);
    let gray = Color::Rgb(0x7C, 0x6F, 0x64);
    let bg0 = Color::Rgb(0xFB, 0xF1, 0xC7);
    let bg1 = Color::Rgb(0xEB, 0xDB, 0xB2);
    let bg_soft = Color::Rgb(0xF2, 0xE5, 0xBC);
    PaletteSlots {
        accent: orange,
        accent_bright: yellow,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg2,
        text_muted: gray,
        border_unfocused: bg1,
        role_name: purple,
        branch_name: aqua,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: bg1,
        selection_fg: fg,
        modal_dim_bg: bg_soft,
        modal_bg: bg0,
        modal_border: orange,
        base_bg: bg0,
    }
    .build()
}

pub(super) fn solarized_light_palette() -> ThemePalette {
    let yellow = Color::Rgb(0xB5, 0x89, 0x00);
    let orange = Color::Rgb(0xCB, 0x4B, 0x16);
    let red = Color::Rgb(0xDC, 0x32, 0x2F);
    let magenta = Color::Rgb(0xD3, 0x36, 0x82);
    let violet = Color::Rgb(0x6C, 0x71, 0xC4);
    let blue = Color::Rgb(0x26, 0x8B, 0xD2);
    let cyan = Color::Rgb(0x2A, 0xA1, 0x98);
    let green = Color::Rgb(0x85, 0x99, 0x00);
    let base00 = Color::Rgb(0x65, 0x7B, 0x83);
    let base01 = Color::Rgb(0x58, 0x6E, 0x75);
    let base1 = Color::Rgb(0x93, 0xA1, 0xA1);
    let base2 = Color::Rgb(0xEE, 0xE8, 0xD5);
    let base3 = Color::Rgb(0xFD, 0xF6, 0xE3);
    PaletteSlots {
        accent: blue,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue,
        text_primary: base01,
        text_secondary: base00,
        text_muted: base1,
        border_unfocused: base2,
        role_name: magenta,
        branch_name: green,
        search_bar: violet,
        keybind_hint: orange,
        selection_bg: base2,
        selection_fg: base01,
        modal_dim_bg: base2,
        modal_bg: base3,
        modal_border: blue,
        base_bg: base3,
    }
    .build()
}

pub(super) fn doom_palette() -> ThemePalette {
    let red = Color::Rgb(0xFF, 0x3B, 0x30);
    let bright_red = Color::Rgb(0xFF, 0x5C, 0x54);
    let bright_green = Color::Rgb(0x6E, 0xFF, 0x6E);
    let cyan = Color::Rgb(0x00, 0xD9, 0xFF);
    // Hellfire amber, not `red`: the yellow slot drives `status_working`, and
    // reusing red there made a working session indistinguishable from a
    // blocked one (both `status_blocked` and `status_error` are red).
    let amber = Color::Rgb(0xFF, 0xA5, 0x00);
    let text = Color::Rgb(0xE0, 0xE0, 0xE0);
    let subtext = Color::Rgb(0xA0, 0xA0, 0xA0);
    let muted = Color::Rgb(0x60, 0x60, 0x60);
    let surface = Color::Rgb(0x2A, 0x0F, 0x12);
    let bg_dark = Color::Rgb(0x1A, 0x08, 0x0A);
    let highlight = Color::Rgb(0x40, 0x15, 0x1E);
    PaletteSlots {
        accent: bright_red,
        accent_bright: bright_green,
        green: bright_green,
        yellow: amber,
        red,
        blue: cyan,
        text_primary: text,
        text_secondary: subtext,
        text_muted: muted,
        border_unfocused: highlight,
        role_name: bright_red,
        branch_name: bright_green,
        search_bar: cyan,
        keybind_hint: amber,
        selection_bg: highlight,
        selection_fg: bright_green,
        modal_dim_bg: bg_dark,
        modal_bg: surface,
        modal_border: bright_red,
        base_bg: bg_dark,
    }
    .build()
}

pub(super) fn nord_palette() -> ThemePalette {
    let frost_cyan = Color::Rgb(0x88, 0xC0, 0xD0); // nord8
    let frost_teal = Color::Rgb(0x8F, 0xBC, 0xBB); // nord7
    let frost_blue = Color::Rgb(0x81, 0xA1, 0xC1); // nord9
    let frost_deep = Color::Rgb(0x5E, 0x81, 0xAC); // nord10
    let green = Color::Rgb(0xA3, 0xBE, 0x8C); // nord14
    let yellow = Color::Rgb(0xEB, 0xCB, 0x8B); // nord13
    let red = Color::Rgb(0xBF, 0x61, 0x6A); // nord11
    let purple = Color::Rgb(0xB4, 0x8E, 0xAD); // nord15
    let snow = Color::Rgb(0xEC, 0xEF, 0xF4); // nord6
    let snow_dim = Color::Rgb(0xD8, 0xDE, 0xE9); // nord4
    let polar3 = Color::Rgb(0x4C, 0x56, 0x6A); // nord3
    let polar2 = Color::Rgb(0x43, 0x4C, 0x5E); // nord2
    let polar1 = Color::Rgb(0x3B, 0x42, 0x52); // nord1
    let polar0 = Color::Rgb(0x2E, 0x34, 0x40); // nord0
    PaletteSlots {
        accent: frost_cyan,
        accent_bright: frost_teal,
        green,
        yellow,
        red,
        blue: frost_blue,
        text_primary: snow,
        text_secondary: snow_dim,
        text_muted: polar3,
        border_unfocused: polar2,
        role_name: purple,
        branch_name: green,
        search_bar: frost_deep,
        keybind_hint: yellow,
        selection_bg: polar2,
        selection_fg: snow,
        modal_dim_bg: polar0,
        modal_bg: polar1,
        modal_border: frost_cyan,
        base_bg: polar0,
    }
    .build()
}

pub(super) fn dracula_palette() -> ThemePalette {
    let purple = Color::Rgb(0xBD, 0x93, 0xF9);
    let pink = Color::Rgb(0xFF, 0x79, 0xC6);
    let green = Color::Rgb(0x50, 0xFA, 0x7B);
    let yellow = Color::Rgb(0xF1, 0xFA, 0x8C);
    let red = Color::Rgb(0xFF, 0x55, 0x55);
    let cyan = Color::Rgb(0x8B, 0xE9, 0xFD);
    let fg = Color::Rgb(0xF8, 0xF8, 0xF2);
    let fg_dim = Color::Rgb(0xC5, 0xC5, 0xD8);
    let comment = Color::Rgb(0x62, 0x72, 0xA4);
    let current_line = Color::Rgb(0x44, 0x47, 0x5A);
    let bg = Color::Rgb(0x28, 0x2A, 0x36);
    let bg_dark = Color::Rgb(0x21, 0x22, 0x2C);
    PaletteSlots {
        accent: purple,
        accent_bright: pink,
        green,
        yellow,
        red,
        blue: cyan,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: comment,
        border_unfocused: current_line,
        role_name: pink,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: yellow,
        selection_bg: current_line,
        selection_fg: fg,
        modal_dim_bg: bg_dark,
        modal_bg: bg,
        modal_border: purple,
        base_bg: bg,
    }
    .build()
}

pub(super) fn one_dark_palette() -> ThemePalette {
    let blue = Color::Rgb(0x61, 0xAF, 0xEF);
    let cyan = Color::Rgb(0x56, 0xB6, 0xC2);
    let green = Color::Rgb(0x98, 0xC3, 0x79);
    let yellow = Color::Rgb(0xE5, 0xC0, 0x7B);
    let red = Color::Rgb(0xE0, 0x6C, 0x75);
    let purple = Color::Rgb(0xC6, 0x78, 0xDD);
    let fg = Color::Rgb(0xAB, 0xB2, 0xBF);
    let fg_dim = Color::Rgb(0x9D, 0xA5, 0xB4);
    let comment = Color::Rgb(0x5C, 0x63, 0x70);
    let gutter = Color::Rgb(0x3E, 0x44, 0x51);
    let bg = Color::Rgb(0x28, 0x2C, 0x34);
    let bg_light = Color::Rgb(0x2C, 0x31, 0x3A);
    let bg_dark = Color::Rgb(0x21, 0x25, 0x2B);
    PaletteSlots {
        accent: blue,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: comment,
        border_unfocused: gutter,
        role_name: purple,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: yellow,
        selection_bg: gutter,
        selection_fg: fg,
        modal_dim_bg: bg_dark,
        modal_bg: bg_light,
        modal_border: blue,
        base_bg: bg,
    }
    .build()
}

pub(super) fn rose_pine_moon_palette() -> ThemePalette {
    let iris = Color::Rgb(0xC4, 0xA7, 0xE7); // purple
    let rose = Color::Rgb(0xEA, 0x9A, 0x97);
    let foam = Color::Rgb(0x9C, 0xCF, 0xD8); // teal/cyan
    let gold = Color::Rgb(0xF6, 0xC1, 0x77); // yellow
    let love = Color::Rgb(0xEB, 0x6F, 0x92); // red/pink
    let pine = Color::Rgb(0x3E, 0x8F, 0xB0); // blue
    let text = Color::Rgb(0xE0, 0xDE, 0xF4);
    let subtle = Color::Rgb(0x90, 0x8C, 0xAA);
    let muted = Color::Rgb(0x6E, 0x6A, 0x86);
    let highlight_med = Color::Rgb(0x44, 0x41, 0x5A);
    let surface = Color::Rgb(0x2A, 0x27, 0x3F);
    let base = Color::Rgb(0x23, 0x21, 0x36);
    PaletteSlots {
        accent: iris,
        accent_bright: rose,
        green: foam,
        yellow: gold,
        red: love,
        blue: pine,
        text_primary: text,
        text_secondary: subtle,
        text_muted: muted,
        border_unfocused: highlight_med,
        role_name: iris,
        branch_name: foam,
        search_bar: pine,
        keybind_hint: gold,
        selection_bg: highlight_med,
        selection_fg: text,
        modal_dim_bg: base,
        modal_bg: surface,
        modal_border: iris,
        base_bg: base,
    }
    .build()
}

pub(super) fn everforest_palette() -> ThemePalette {
    let green = Color::Rgb(0xA7, 0xC0, 0x80);
    let aqua = Color::Rgb(0x83, 0xC0, 0x92);
    let yellow = Color::Rgb(0xDB, 0xBC, 0x7F);
    let red = Color::Rgb(0xE6, 0x7E, 0x80);
    let blue = Color::Rgb(0x7F, 0xBB, 0xB3);
    let purple = Color::Rgb(0xD6, 0x99, 0xB6);
    let fg = Color::Rgb(0xD3, 0xC6, 0xAA);
    let grey1 = Color::Rgb(0x9D, 0xA9, 0xA0);
    let grey0 = Color::Rgb(0x7A, 0x84, 0x78);
    let bg2 = Color::Rgb(0x3D, 0x48, 0x4D);
    let bg1 = Color::Rgb(0x34, 0x3F, 0x44);
    let bg0 = Color::Rgb(0x2D, 0x35, 0x3B);
    let bg_dim = Color::Rgb(0x23, 0x2A, 0x2E);
    PaletteSlots {
        accent: green,
        accent_bright: aqua,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: grey1,
        text_muted: grey0,
        border_unfocused: bg2,
        role_name: purple,
        branch_name: aqua,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: bg2,
        selection_fg: fg,
        modal_dim_bg: bg_dim,
        modal_bg: bg1,
        modal_border: green,
        base_bg: bg0,
    }
    .build()
}

pub(super) fn kanagawa_palette() -> ThemePalette {
    let crystal_blue = Color::Rgb(0x7E, 0x9C, 0xD8);
    let spring_blue = Color::Rgb(0x7F, 0xB4, 0xCA); // cyan
    let spring_green = Color::Rgb(0x98, 0xBB, 0x6C);
    let carp_yellow = Color::Rgb(0xE6, 0xC3, 0x84);
    let wave_red = Color::Rgb(0xE4, 0x68, 0x76);
    let oni_violet = Color::Rgb(0x95, 0x7F, 0xB8);
    let fuji_white = Color::Rgb(0xDC, 0xD7, 0xBA);
    let old_white = Color::Rgb(0xC8, 0xC0, 0x93);
    let fuji_gray = Color::Rgb(0x72, 0x71, 0x69);
    let sumi_ink3 = Color::Rgb(0x36, 0x36, 0x46);
    let wave_blue2 = Color::Rgb(0x2D, 0x4F, 0x67);
    let sumi_ink2 = Color::Rgb(0x2A, 0x2A, 0x37);
    let sumi_ink1 = Color::Rgb(0x1F, 0x1F, 0x28);
    let sumi_ink0 = Color::Rgb(0x16, 0x16, 0x1D);
    PaletteSlots {
        accent: crystal_blue,
        accent_bright: spring_blue,
        green: spring_green,
        yellow: carp_yellow,
        red: wave_red,
        blue: crystal_blue,
        text_primary: fuji_white,
        text_secondary: old_white,
        text_muted: fuji_gray,
        border_unfocused: sumi_ink3,
        role_name: oni_violet,
        branch_name: spring_green,
        search_bar: spring_blue,
        keybind_hint: carp_yellow,
        selection_bg: wave_blue2,
        selection_fg: fuji_white,
        modal_dim_bg: sumi_ink0,
        modal_bg: sumi_ink2,
        modal_border: crystal_blue,
        base_bg: sumi_ink1,
    }
    .build()
}

pub(super) fn solarized_dark_palette() -> ThemePalette {
    let yellow = Color::Rgb(0xB5, 0x89, 0x00);
    let orange = Color::Rgb(0xCB, 0x4B, 0x16);
    let red = Color::Rgb(0xDC, 0x32, 0x2F);
    let magenta = Color::Rgb(0xD3, 0x36, 0x82);
    let violet = Color::Rgb(0x6C, 0x71, 0xC4);
    let blue = Color::Rgb(0x26, 0x8B, 0xD2);
    let cyan = Color::Rgb(0x2A, 0xA1, 0x98);
    let green = Color::Rgb(0x85, 0x99, 0x00);
    let base03 = Color::Rgb(0x00, 0x2B, 0x36);
    let base02 = Color::Rgb(0x07, 0x36, 0x42);
    let base01 = Color::Rgb(0x58, 0x6E, 0x75);
    let base0 = Color::Rgb(0x83, 0x94, 0x96);
    let base1 = Color::Rgb(0x93, 0xA1, 0xA1);
    PaletteSlots {
        // Cyan accent keeps Solarized Dark distinct from Solarized Light,
        // which takes the blue.
        accent: cyan,
        accent_bright: blue,
        green,
        yellow,
        red,
        blue,
        text_primary: base1,
        text_secondary: base0,
        text_muted: base01,
        border_unfocused: base02,
        role_name: magenta,
        branch_name: green,
        search_bar: violet,
        keybind_hint: orange,
        selection_bg: base02,
        selection_fg: base1,
        modal_dim_bg: base03,
        modal_bg: base02,
        modal_border: cyan,
        base_bg: base03,
    }
    .build()
}

pub(super) fn monokai_palette() -> ThemePalette {
    let pink = Color::Rgb(0xF9, 0x26, 0x72);
    let green = Color::Rgb(0xA6, 0xE2, 0x2E);
    let yellow = Color::Rgb(0xE6, 0xDB, 0x74);
    let orange = Color::Rgb(0xFD, 0x97, 0x1F);
    let purple = Color::Rgb(0xAE, 0x81, 0xFF);
    let cyan = Color::Rgb(0x66, 0xD9, 0xEF);
    let fg = Color::Rgb(0xF8, 0xF8, 0xF2);
    let fg_dim = Color::Rgb(0xCF, 0xCF, 0xC2);
    let comment = Color::Rgb(0x75, 0x71, 0x5E);
    let bg_light = Color::Rgb(0x3E, 0x3D, 0x32);
    let bg = Color::Rgb(0x27, 0x28, 0x22);
    let bg_dark = Color::Rgb(0x1D, 0x1E, 0x19);
    PaletteSlots {
        accent: pink,
        accent_bright: orange,
        green,
        yellow,
        red: pink,
        blue: cyan,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: comment,
        border_unfocused: bg_light,
        role_name: purple,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: yellow,
        selection_bg: bg_light,
        selection_fg: fg,
        modal_dim_bg: bg_dark,
        modal_bg: bg_light,
        modal_border: pink,
        base_bg: bg,
    }
    .build()
}

pub(super) fn ayu_dark_palette() -> ThemePalette {
    let orange = Color::Rgb(0xFF, 0xB4, 0x54);
    let amber = Color::Rgb(0xE6, 0xB4, 0x50);
    let green = Color::Rgb(0xAA, 0xD9, 0x4C);
    let red = Color::Rgb(0xF0, 0x71, 0x78);
    let blue = Color::Rgb(0x59, 0xC2, 0xFF);
    let purple = Color::Rgb(0xD2, 0xA6, 0xFF);
    let fg = Color::Rgb(0xBF, 0xBD, 0xB6);
    let fg_dim = Color::Rgb(0x9B, 0x99, 0x93);
    let muted = Color::Rgb(0x5C, 0x61, 0x66);
    let panel = Color::Rgb(0x1F, 0x24, 0x30);
    let line = Color::Rgb(0x25, 0x2B, 0x38);
    let bg = Color::Rgb(0x0B, 0x0E, 0x14);
    PaletteSlots {
        accent: orange,
        accent_bright: amber,
        green,
        yellow: amber,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: line,
        role_name: purple,
        branch_name: green,
        search_bar: blue,
        keybind_hint: amber,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: bg,
        modal_bg: panel,
        modal_border: orange,
        base_bg: bg,
    }
    .build()
}

pub(super) fn ayu_mirage_palette() -> ThemePalette {
    let orange = Color::Rgb(0xFF, 0xCC, 0x66);
    let amber = Color::Rgb(0xFF, 0xD1, 0x73);
    let green = Color::Rgb(0xD5, 0xFF, 0x80);
    let red = Color::Rgb(0xF2, 0x87, 0x79);
    let blue = Color::Rgb(0x73, 0xD0, 0xFF);
    let purple = Color::Rgb(0xDF, 0xBF, 0xFF);
    let fg = Color::Rgb(0xCC, 0xCA, 0xC2);
    let fg_dim = Color::Rgb(0xA6, 0xAC, 0xB9);
    let muted = Color::Rgb(0x70, 0x76, 0x84);
    let panel = Color::Rgb(0x1F, 0x24, 0x30);
    let line = Color::Rgb(0x2D, 0x34, 0x40);
    let bg = Color::Rgb(0x24, 0x29, 0x36);
    PaletteSlots {
        accent: amber,
        accent_bright: green,
        green,
        yellow: orange,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: line,
        role_name: purple,
        branch_name: green,
        search_bar: blue,
        keybind_hint: orange,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: panel,
        modal_bg: line,
        modal_border: amber,
        base_bg: bg,
    }
    .build()
}

pub(super) fn material_palette() -> ThemePalette {
    let cyan = Color::Rgb(0x89, 0xDD, 0xFF);
    let teal = Color::Rgb(0x80, 0xCB, 0xC4);
    let green = Color::Rgb(0xC3, 0xE8, 0x8D);
    let yellow = Color::Rgb(0xFF, 0xCB, 0x6B);
    let red = Color::Rgb(0xF0, 0x71, 0x78);
    let blue = Color::Rgb(0x82, 0xAA, 0xFF);
    let purple = Color::Rgb(0xC7, 0x92, 0xEA);
    let fg = Color::Rgb(0xEE, 0xFF, 0xFF);
    let fg_dim = Color::Rgb(0xB2, 0xCC, 0xD6);
    let comment = Color::Rgb(0x54, 0x6E, 0x7A);
    let line = Color::Rgb(0x31, 0x36, 0x3B);
    let surface = Color::Rgb(0x26, 0x2A, 0x2E);
    let bg = Color::Rgb(0x21, 0x25, 0x29);
    PaletteSlots {
        accent: teal,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: comment,
        border_unfocused: line,
        role_name: purple,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: bg,
        modal_bg: surface,
        modal_border: teal,
        base_bg: bg,
    }
    .build()
}

pub(super) fn rose_pine_palette() -> ThemePalette {
    let iris = Color::Rgb(0xC4, 0xA7, 0xE7);
    let rose = Color::Rgb(0xEB, 0xBC, 0xBA);
    let foam = Color::Rgb(0x9C, 0xCF, 0xD8);
    let gold = Color::Rgb(0xF6, 0xC1, 0x77);
    let love = Color::Rgb(0xEB, 0x6F, 0x92);
    let pine = Color::Rgb(0x31, 0x74, 0x8F);
    let text = Color::Rgb(0xE0, 0xDE, 0xF4);
    let subtle = Color::Rgb(0x90, 0x8C, 0xAA);
    let muted = Color::Rgb(0x6E, 0x6A, 0x86);
    let highlight_med = Color::Rgb(0x40, 0x3D, 0x52);
    let surface = Color::Rgb(0x1F, 0x1D, 0x2E);
    let base = Color::Rgb(0x19, 0x17, 0x24);
    PaletteSlots {
        // The Moon variant already takes iris; use rose here so the two
        // Rosé Pine flavours read differently in the picker swatch.
        accent: rose,
        accent_bright: iris,
        green: foam,
        yellow: gold,
        red: love,
        blue: pine,
        text_primary: text,
        text_secondary: subtle,
        text_muted: muted,
        border_unfocused: highlight_med,
        role_name: iris,
        branch_name: foam,
        search_bar: pine,
        keybind_hint: gold,
        selection_bg: highlight_med,
        selection_fg: text,
        modal_dim_bg: base,
        modal_bg: surface,
        modal_border: rose,
        base_bg: base,
    }
    .build()
}

pub(super) fn oxocarbon_palette() -> ThemePalette {
    let cyan = Color::Rgb(0x3D, 0xDB, 0xD9);
    let blue = Color::Rgb(0x78, 0xA9, 0xFF);
    let purple = Color::Rgb(0xBE, 0x95, 0xFF);
    let magenta = Color::Rgb(0xFF, 0x7E, 0xB6);
    let green = Color::Rgb(0x42, 0xBE, 0x65);
    let yellow = Color::Rgb(0xFF, 0xE9, 0x7C);
    let red = Color::Rgb(0xEE, 0x53, 0x96);
    let fg = Color::Rgb(0xF2, 0xF4, 0xF8);
    let fg_dim = Color::Rgb(0xC6, 0xC6, 0xC6);
    let muted = Color::Rgb(0x6F, 0x6F, 0x6F);
    let gray80 = Color::Rgb(0x39, 0x39, 0x39);
    let gray90 = Color::Rgb(0x26, 0x26, 0x26);
    let gray100 = Color::Rgb(0x16, 0x16, 0x16);
    PaletteSlots {
        accent: cyan,
        accent_bright: magenta,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: gray80,
        role_name: purple,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: gray80,
        selection_fg: fg,
        modal_dim_bg: gray100,
        modal_bg: gray90,
        modal_border: cyan,
        base_bg: gray100,
    }
    .build()
}

pub(super) fn github_dark_palette() -> ThemePalette {
    let blue = Color::Rgb(0x58, 0xA6, 0xFF);
    let cyan = Color::Rgb(0x76, 0xE3, 0xEA);
    let green = Color::Rgb(0x3F, 0xB9, 0x50);
    let yellow = Color::Rgb(0xD2, 0x99, 0x22);
    let red = Color::Rgb(0xF8, 0x51, 0x49);
    let purple = Color::Rgb(0xBC, 0x8C, 0xFF);
    let fg = Color::Rgb(0xC9, 0xD1, 0xD9);
    let fg_dim = Color::Rgb(0xB1, 0xBA, 0xC4);
    let muted = Color::Rgb(0x6E, 0x76, 0x81);
    let border = Color::Rgb(0x30, 0x36, 0x3D);
    let canvas_subtle = Color::Rgb(0x16, 0x1B, 0x22);
    let canvas = Color::Rgb(0x0D, 0x11, 0x17);
    PaletteSlots {
        accent: blue,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: border,
        role_name: purple,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: yellow,
        selection_bg: border,
        selection_fg: fg,
        modal_dim_bg: canvas,
        modal_bg: canvas_subtle,
        modal_border: blue,
        base_bg: canvas,
    }
    .build()
}

pub(super) fn nightfox_palette() -> ThemePalette {
    let blue = Color::Rgb(0x71, 0x9C, 0xD6);
    let cyan = Color::Rgb(0x63, 0xCD, 0xCF);
    let green = Color::Rgb(0x81, 0xB2, 0x9A);
    let yellow = Color::Rgb(0xDB, 0xC0, 0x74);
    let red = Color::Rgb(0xC9, 0x4F, 0x6D);
    let magenta = Color::Rgb(0x9D, 0x79, 0xD6);
    let fg = Color::Rgb(0xCD, 0xCE, 0xCF);
    let fg_dim = Color::Rgb(0xAE, 0xAF, 0xB0);
    let muted = Color::Rgb(0x73, 0x82, 0x91);
    let sel = Color::Rgb(0x2B, 0x3B, 0x51);
    let bg1 = Color::Rgb(0x19, 0x24, 0x30);
    let bg0 = Color::Rgb(0x13, 0x1A, 0x24);
    PaletteSlots {
        accent: cyan,
        accent_bright: blue,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: sel,
        role_name: magenta,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: sel,
        selection_fg: fg,
        modal_dim_bg: bg0,
        modal_bg: bg1,
        modal_border: cyan,
        base_bg: bg0,
    }
    .build()
}

pub(super) fn sonokai_palette() -> ThemePalette {
    let red = Color::Rgb(0xFC, 0x5D, 0x7C);
    let orange = Color::Rgb(0xF3, 0x96, 0x60);
    let yellow = Color::Rgb(0xE7, 0xC6, 0x64);
    let green = Color::Rgb(0x9E, 0xD0, 0x72);
    let blue = Color::Rgb(0x76, 0xCC, 0xE0);
    let purple = Color::Rgb(0xB3, 0x9D, 0xF3);
    let fg = Color::Rgb(0xE2, 0xE2, 0xE3);
    let fg_dim = Color::Rgb(0xC1, 0xC1, 0xC3);
    let grey = Color::Rgb(0x7F, 0x84, 0x90);
    let bg4 = Color::Rgb(0x3B, 0x3E, 0x48);
    let bg2 = Color::Rgb(0x33, 0x35, 0x3F);
    let bg0 = Color::Rgb(0x2C, 0x2E, 0x34);
    PaletteSlots {
        accent: orange,
        accent_bright: yellow,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: grey,
        border_unfocused: bg4,
        role_name: purple,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: bg4,
        selection_fg: fg,
        modal_dim_bg: bg0,
        modal_bg: bg2,
        modal_border: orange,
        base_bg: bg0,
    }
    .build()
}

pub(super) fn melange_palette() -> ThemePalette {
    let tan = Color::Rgb(0xEB, 0xC0, 0x6D);
    let peach = Color::Rgb(0xD4, 0x77, 0x66);
    let green = Color::Rgb(0x85, 0xB6, 0x95);
    let yellow = Color::Rgb(0xEB, 0xC0, 0x6D);
    let red = Color::Rgb(0xD4, 0x73, 0x66);
    let blue = Color::Rgb(0xA3, 0xA9, 0xCE);
    let purple = Color::Rgb(0xCF, 0x9B, 0xC2);
    let fg = Color::Rgb(0xEC, 0xE1, 0xD7);
    let fg_dim = Color::Rgb(0xC1, 0xB2, 0xA4);
    let muted = Color::Rgb(0x86, 0x78, 0x6D);
    let sel = Color::Rgb(0x40, 0x36, 0x30);
    let surface = Color::Rgb(0x33, 0x2C, 0x28);
    let bg = Color::Rgb(0x29, 0x24, 0x22);
    PaletteSlots {
        accent: tan,
        accent_bright: peach,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: sel,
        role_name: purple,
        branch_name: green,
        search_bar: blue,
        keybind_hint: peach,
        selection_bg: sel,
        selection_fg: fg,
        modal_dim_bg: bg,
        modal_bg: surface,
        modal_border: tan,
        base_bg: bg,
    }
    .build()
}

pub(super) fn zenburn_palette() -> ThemePalette {
    let sand = Color::Rgb(0xF0, 0xDF, 0xAF);
    let orange = Color::Rgb(0xDF, 0xAF, 0x8F);
    let green = Color::Rgb(0x7F, 0x9F, 0x7F);
    let yellow = Color::Rgb(0xE3, 0xCE, 0xAB);
    let red = Color::Rgb(0xCC, 0x93, 0x93);
    let blue = Color::Rgb(0x8C, 0xD0, 0xD3);
    let purple = Color::Rgb(0xDC, 0x8C, 0xC3);
    let fg = Color::Rgb(0xDC, 0xDC, 0xCC);
    let fg_dim = Color::Rgb(0xC0, 0xC0, 0xB0);
    let muted = Color::Rgb(0x70, 0x80, 0x70);
    let bg_plus2 = Color::Rgb(0x4F, 0x4F, 0x4F);
    let bg_plus1 = Color::Rgb(0x40, 0x40, 0x40);
    let bg = Color::Rgb(0x3F, 0x3F, 0x3F);
    let bg_minus = Color::Rgb(0x2B, 0x2B, 0x2B);
    PaletteSlots {
        accent: sand,
        accent_bright: orange,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: bg_plus2,
        role_name: purple,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: bg_plus2,
        selection_fg: fg,
        modal_dim_bg: bg_minus,
        modal_bg: bg_plus1,
        modal_border: sand,
        base_bg: bg,
    }
    .build()
}

pub(super) fn iceberg_palette() -> ThemePalette {
    let blue = Color::Rgb(0x84, 0xA0, 0xC6);
    let cyan = Color::Rgb(0x89, 0xB8, 0xC2);
    let green = Color::Rgb(0xB4, 0xBE, 0x82);
    let yellow = Color::Rgb(0xE2, 0xA4, 0x78);
    let red = Color::Rgb(0xE2, 0x78, 0x78);
    let purple = Color::Rgb(0xA0, 0x93, 0xC7);
    let fg = Color::Rgb(0xC6, 0xC8, 0xD1);
    let fg_dim = Color::Rgb(0xAD, 0xB1, 0xC4);
    let muted = Color::Rgb(0x6B, 0x70, 0x89);
    let line = Color::Rgb(0x3E, 0x44, 0x51);
    let surface = Color::Rgb(0x1E, 0x21, 0x32);
    let bg = Color::Rgb(0x16, 0x18, 0x21);
    PaletteSlots {
        accent: blue,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: line,
        role_name: purple,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: yellow,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: bg,
        modal_bg: surface,
        modal_border: blue,
        base_bg: bg,
    }
    .build()
}

pub(super) fn vesper_palette() -> ThemePalette {
    let amber = Color::Rgb(0xFF, 0xC7, 0x7B);
    let peach = Color::Rgb(0xFF, 0xA8, 0x5C);
    let green = Color::Rgb(0x99, 0xFF, 0xE4);
    let yellow = Color::Rgb(0xFF, 0xCF, 0xA8);
    let red = Color::Rgb(0xFF, 0x80, 0x80);
    let blue = Color::Rgb(0xA0, 0xA0, 0xA0);
    let fg = Color::Rgb(0xFF, 0xFF, 0xFF);
    let fg_dim = Color::Rgb(0xA0, 0xA0, 0xA0);
    let muted = Color::Rgb(0x50, 0x50, 0x50);
    let line = Color::Rgb(0x2A, 0x2A, 0x2A);
    let surface = Color::Rgb(0x18, 0x18, 0x18);
    let bg = Color::Rgb(0x10, 0x10, 0x10);
    PaletteSlots {
        accent: amber,
        accent_bright: peach,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: line,
        role_name: peach,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: bg,
        modal_bg: surface,
        modal_border: amber,
        base_bg: bg,
    }
    .build()
}

pub(super) fn synthwave_palette() -> ThemePalette {
    let magenta = Color::Rgb(0xFF, 0x7E, 0xDB);
    let cyan = Color::Rgb(0x36, 0xF9, 0xF6);
    let green = Color::Rgb(0x72, 0xF1, 0xB8);
    let yellow = Color::Rgb(0xFE, 0xDE, 0x5D);
    let red = Color::Rgb(0xFE, 0x44, 0x50);
    let orange = Color::Rgb(0xFF, 0x8B, 0x39);
    let fg = Color::Rgb(0xF9, 0xF9, 0xF9);
    let fg_dim = Color::Rgb(0xD3, 0xC5, 0xE8);
    let muted = Color::Rgb(0x84, 0x8B, 0xBD);
    let line = Color::Rgb(0x34, 0x29, 0x4F);
    let surface = Color::Rgb(0x26, 0x1E, 0x3C);
    let bg = Color::Rgb(0x26, 0x22, 0x35);
    PaletteSlots {
        accent: magenta,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue: cyan,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: line,
        role_name: orange,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: yellow,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: surface,
        modal_bg: line,
        modal_border: magenta,
        base_bg: bg,
    }
    .build()
}

pub(super) fn nightfly_palette() -> ThemePalette {
    let blue = Color::Rgb(0x82, 0xAA, 0xFF);
    let cyan = Color::Rgb(0x7F, 0xDB, 0xCA);
    let green = Color::Rgb(0xA1, 0xCD, 0x5E);
    let yellow = Color::Rgb(0xE3, 0xD1, 0x8A);
    let red = Color::Rgb(0xFC, 0x51, 0x4E);
    let purple = Color::Rgb(0xAE, 0x81, 0xFF);
    let fg = Color::Rgb(0xBD, 0xC1, 0xC6);
    let fg_dim = Color::Rgb(0xA1, 0xAA, 0xB8);
    let muted = Color::Rgb(0x63, 0x6D, 0x83);
    let line = Color::Rgb(0x2F, 0x3B, 0x54);
    let surface = Color::Rgb(0x1D, 0x30, 0x43);
    let bg = Color::Rgb(0x01, 0x16, 0x27);
    PaletteSlots {
        accent: cyan,
        accent_bright: blue,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: line,
        role_name: purple,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: bg,
        modal_bg: surface,
        modal_border: cyan,
        base_bg: bg,
    }
    .build()
}

pub(super) fn tomorrow_night_palette() -> ThemePalette {
    let blue = Color::Rgb(0x81, 0xA2, 0xBE);
    let aqua = Color::Rgb(0x8A, 0xBE, 0xB7);
    let green = Color::Rgb(0xB5, 0xBD, 0x68);
    let yellow = Color::Rgb(0xF0, 0xC6, 0x74);
    let red = Color::Rgb(0xCC, 0x66, 0x66);
    let purple = Color::Rgb(0xB2, 0x94, 0xBB);
    let orange = Color::Rgb(0xDE, 0x93, 0x5F);
    let fg = Color::Rgb(0xC5, 0xC8, 0xC6);
    let fg_dim = Color::Rgb(0xB4, 0xB7, 0xB4);
    let comment = Color::Rgb(0x96, 0x98, 0x96);
    let line = Color::Rgb(0x37, 0x3B, 0x41);
    let surface = Color::Rgb(0x28, 0x2A, 0x2E);
    let bg = Color::Rgb(0x1D, 0x1F, 0x21);
    PaletteSlots {
        accent: blue,
        accent_bright: aqua,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: comment,
        border_unfocused: line,
        role_name: purple,
        branch_name: green,
        search_bar: aqua,
        keybind_hint: orange,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: bg,
        modal_bg: surface,
        modal_border: blue,
        base_bg: bg,
    }
    .build()
}

pub(super) fn ayu_light_palette() -> ThemePalette {
    let orange = Color::Rgb(0xFA, 0x8D, 0x3E);
    let amber = Color::Rgb(0xE6, 0xBA, 0x7E);
    let green = Color::Rgb(0x6C, 0xA3, 0x00);
    let yellow = Color::Rgb(0xA3, 0x71, 0x00);
    let red = Color::Rgb(0xF0, 0x71, 0x71);
    let blue = Color::Rgb(0x39, 0x9E, 0xE6);
    let purple = Color::Rgb(0xA3, 0x7A, 0xCC);
    let fg = Color::Rgb(0x5C, 0x61, 0x66);
    let fg_dim = Color::Rgb(0x78, 0x7B, 0x80);
    let muted = Color::Rgb(0x8A, 0x91, 0x99);
    let line = Color::Rgb(0xE7, 0xE8, 0xE9);
    let panel = Color::Rgb(0xF3, 0xF4, 0xF5);
    let bg = Color::Rgb(0xFC, 0xFC, 0xFC);
    PaletteSlots {
        accent: orange,
        accent_bright: amber,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: line,
        role_name: purple,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: panel,
        modal_bg: bg,
        modal_border: orange,
        base_bg: bg,
    }
    .build()
}

pub(super) fn one_light_palette() -> ThemePalette {
    let blue = Color::Rgb(0x40, 0x78, 0xF2);
    let cyan = Color::Rgb(0x01, 0x84, 0xBC);
    let green = Color::Rgb(0x50, 0xA1, 0x4F);
    let yellow = Color::Rgb(0xC1, 0x84, 0x01);
    let red = Color::Rgb(0xE4, 0x56, 0x49);
    let purple = Color::Rgb(0xA6, 0x26, 0xA4);
    let fg = Color::Rgb(0x38, 0x3A, 0x42);
    let fg_dim = Color::Rgb(0x50, 0x53, 0x5C);
    let muted = Color::Rgb(0x9D, 0xA5, 0xB4);
    let line = Color::Rgb(0xE5, 0xE5, 0xE6);
    let panel = Color::Rgb(0xEA, 0xEA, 0xEB);
    let bg = Color::Rgb(0xFA, 0xFA, 0xFA);
    PaletteSlots {
        accent: blue,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: line,
        role_name: purple,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: yellow,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: panel,
        modal_bg: bg,
        modal_border: blue,
        base_bg: bg,
    }
    .build()
}

pub(super) fn rose_pine_dawn_palette() -> ThemePalette {
    let iris = Color::Rgb(0x90, 0x7A, 0xA9);
    let rose = Color::Rgb(0xD7, 0x82, 0x7E);
    let foam = Color::Rgb(0x56, 0x94, 0x9F);
    let gold = Color::Rgb(0xEA, 0x9D, 0x34);
    let love = Color::Rgb(0xB4, 0x63, 0x7A);
    let pine = Color::Rgb(0x28, 0x69, 0x83);
    let text = Color::Rgb(0x57, 0x52, 0x79);
    let subtle = Color::Rgb(0x79, 0x75, 0x93);
    let muted = Color::Rgb(0x98, 0x93, 0xA5);
    let highlight_med = Color::Rgb(0xDF, 0xDA, 0xD9);
    let surface = Color::Rgb(0xFF, 0xFA, 0xF3);
    let base = Color::Rgb(0xFA, 0xF4, 0xED);
    PaletteSlots {
        accent: iris,
        accent_bright: rose,
        green: foam,
        yellow: gold,
        red: love,
        blue: pine,
        text_primary: text,
        text_secondary: subtle,
        text_muted: muted,
        border_unfocused: highlight_med,
        role_name: iris,
        branch_name: foam,
        search_bar: pine,
        keybind_hint: gold,
        selection_bg: highlight_med,
        selection_fg: text,
        modal_dim_bg: highlight_med,
        modal_bg: surface,
        modal_border: iris,
        base_bg: base,
    }
    .build()
}

pub(super) fn github_light_palette() -> ThemePalette {
    let blue = Color::Rgb(0x09, 0x69, 0xDA);
    let cyan = Color::Rgb(0x1B, 0x7C, 0x83);
    let green = Color::Rgb(0x1A, 0x7F, 0x37);
    let yellow = Color::Rgb(0x95, 0x6C, 0x05);
    let red = Color::Rgb(0xCF, 0x22, 0x2E);
    let purple = Color::Rgb(0x82, 0x50, 0xDF);
    let fg = Color::Rgb(0x1F, 0x23, 0x28);
    let fg_dim = Color::Rgb(0x42, 0x4A, 0x53);
    let muted = Color::Rgb(0x6E, 0x77, 0x81);
    let border = Color::Rgb(0xD0, 0xD7, 0xDE);
    let canvas_subtle = Color::Rgb(0xF6, 0xF8, 0xFA);
    let canvas = Color::Rgb(0xFF, 0xFF, 0xFF);
    PaletteSlots {
        accent: blue,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: border,
        role_name: purple,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: yellow,
        selection_bg: border,
        selection_fg: fg,
        modal_dim_bg: canvas_subtle,
        modal_bg: canvas,
        modal_border: blue,
        base_bg: canvas,
    }
    .build()
}
