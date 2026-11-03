//! Termius 风格 6 套主题色板与 egui Visuals 映射。

use eframe::egui::{self, Color32, CornerRadius, Stroke, Visuals};

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct Palette {
    pub id: &'static str,
    pub name: &'static str,
    pub dark: bool,
    pub bg: Color32,
    pub bg_elev: Color32,
    pub panel: Color32,
    pub surface2: Color32,
    pub surface3: Color32,
    pub border: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub faint: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub ok: Color32,
    pub ok_soft: Color32,
    pub warn: Color32,
    pub warn_soft: Color32,
    pub danger: Color32,
    pub danger_soft: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub const PALETTES: [Palette; 6] = [
    Palette {
        id: "obsidian",
        name: "曜石 (Termius)",
        dark: true,
        bg: rgb(0x0B, 0x0D, 0x12),
        bg_elev: rgb(0x10, 0x14, 0x1D),
        panel: rgb(0x14, 0x19, 0x23),
        surface2: rgb(0x0E, 0x12, 0x1A),
        surface3: rgb(0x1B, 0x22, 0x30),
        border: rgb(0x21, 0x29, 0x38),
        text: rgb(0xE6, 0xED, 0xF3),
        dim: rgb(0x9A, 0xA7, 0xB6),
        faint: rgb(0x63, 0x70, 0x81),
        accent: rgb(0x3B, 0x82, 0xF6),
        accent_soft: rgb(0x15, 0x25, 0x40),
        ok: rgb(0x10, 0xB9, 0x81),
        ok_soft: rgb(0x0F, 0x29, 0x22),
        warn: rgb(0xD6, 0xA0, 0x34),
        warn_soft: rgb(0x2A, 0x24, 0x18),
        danger: rgb(0xF8, 0x51, 0x49),
        danger_soft: rgb(0x2A, 0x16, 0x18),
    },
    Palette {
        id: "midnight",
        name: "午夜",
        dark: true,
        bg: rgb(0x0A, 0x10, 0x20),
        bg_elev: rgb(0x10, 0x1A, 0x2E),
        panel: rgb(0x0E, 0x17, 0x28),
        surface2: rgb(0x0B, 0x13, 0x22),
        surface3: rgb(0x17, 0x23, 0x3A),
        border: rgb(0x22, 0x30, 0x4A),
        text: rgb(0xE4, 0xEC, 0xF8),
        dim: rgb(0x97, 0xA8, 0xC0),
        faint: rgb(0x64, 0x74, 0x8F),
        accent: rgb(0x6A, 0xA6, 0xFF),
        accent_soft: rgb(0x17, 0x27, 0x4A),
        ok: rgb(0x41, 0xC4, 0x83),
        ok_soft: rgb(0x12, 0x2A, 0x22),
        warn: rgb(0xDF, 0xA8, 0x3C),
        warn_soft: rgb(0x2C, 0x25, 0x18),
        danger: rgb(0xF0, 0x6F, 0x67),
        danger_soft: rgb(0x2E, 0x18, 0x1C),
    },
    Palette {
        id: "graphite",
        name: "石墨",
        dark: true,
        bg: rgb(0x16, 0x17, 0x1A),
        bg_elev: rgb(0x1E, 0x20, 0x24),
        panel: rgb(0x1A, 0x1C, 0x20),
        surface2: rgb(0x14, 0x15, 0x18),
        surface3: rgb(0x25, 0x28, 0x2E),
        border: rgb(0x2E, 0x31, 0x38),
        text: rgb(0xE3, 0xE5, 0xEA),
        dim: rgb(0x9E, 0xA3, 0xAE),
        faint: rgb(0x6E, 0x73, 0x7E),
        accent: rgb(0x6A, 0x9F, 0xF5),
        accent_soft: rgb(0x20, 0x2B, 0x40),
        ok: rgb(0x4A, 0xBE, 0x82),
        ok_soft: rgb(0x1A, 0x2B, 0x23),
        warn: rgb(0xD4, 0xA0, 0x3D),
        warn_soft: rgb(0x2D, 0x27, 0x1B),
        danger: rgb(0xE0, 0x6A, 0x62),
        danger_soft: rgb(0x2F, 0x1D, 0x1D),
    },
    Palette {
        id: "cloud",
        name: "云白",
        dark: false,
        bg: rgb(0xF5, 0xF7, 0xFA),
        bg_elev: rgb(0xFF, 0xFF, 0xFF),
        panel: rgb(0xFF, 0xFF, 0xFF),
        surface2: rgb(0xEE, 0xF1, 0xF6),
        surface3: rgb(0xE6, 0xEA, 0xF1),
        border: rgb(0xDC, 0xE1, 0xEA),
        text: rgb(0x1C, 0x22, 0x2B),
        dim: rgb(0x55, 0x60, 0x70),
        faint: rgb(0x88, 0x92, 0xA2),
        accent: rgb(0x25, 0x63, 0xEB),
        accent_soft: rgb(0xE6, 0xEE, 0xFD),
        ok: rgb(0x15, 0x80, 0x3D),
        ok_soft: rgb(0xE3, 0xF5, 0xEA),
        warn: rgb(0xB4, 0x53, 0x09),
        warn_soft: rgb(0xFB, 0xF0, 0xDC),
        danger: rgb(0xDC, 0x26, 0x26),
        danger_soft: rgb(0xFC, 0xE8, 0xE8),
    },
    Palette {
        id: "linen",
        name: "亚麻",
        dark: false,
        bg: rgb(0xF6, 0xF3, 0xEC),
        bg_elev: rgb(0xFD, 0xFB, 0xF6),
        panel: rgb(0xFC, 0xF9, 0xF2),
        surface2: rgb(0xEF, 0xEB, 0xE1),
        surface3: rgb(0xE7, 0xE1, 0xD5),
        border: rgb(0xDC, 0xD5, 0xC6),
        text: rgb(0x26, 0x23, 0x1E),
        dim: rgb(0x62, 0x5B, 0x4F),
        faint: rgb(0x91, 0x89, 0x7B),
        accent: rgb(0x2B, 0x5F, 0xB8),
        accent_soft: rgb(0xE3, 0xEA, 0xF6),
        ok: rgb(0x1F, 0x7A, 0x45),
        ok_soft: rgb(0xE1, 0xF0, 0xE6),
        warn: rgb(0xA8, 0x58, 0x0C),
        warn_soft: rgb(0xF6, 0xE9, 0xD3),
        danger: rgb(0xC9, 0x34, 0x2D),
        danger_soft: rgb(0xF8, 0xE3, 0xE1),
    },
    Palette {
        id: "mist",
        name: "雾青",
        dark: false,
        bg: rgb(0xEE, 0xF3, 0xF4),
        bg_elev: rgb(0xF9, 0xFC, 0xFC),
        panel: rgb(0xF7, 0xFA, 0xFB),
        surface2: rgb(0xE5, 0xEC, 0xEE),
        surface3: rgb(0xDC, 0xE5, 0xE8),
        border: rgb(0xCE, 0xD9, 0xDD),
        text: rgb(0x1B, 0x26, 0x2B),
        dim: rgb(0x4F, 0x61, 0x68),
        faint: rgb(0x7E, 0x90, 0x97),
        accent: rgb(0x19, 0x67, 0x85),
        accent_soft: rgb(0xDC, 0xEE, 0xF4),
        ok: rgb(0x1C, 0x7C, 0x54),
        ok_soft: rgb(0xDE, 0xF1, 0xE8),
        warn: rgb(0xAA, 0x62, 0x12),
        warn_soft: rgb(0xF5, 0xEB, 0xD7),
        danger: rgb(0xC7, 0x38, 0x38),
        danger_soft: rgb(0xF8, 0xE3, 0xE3),
    },
];

pub const THEME_CHOICES: [(&str, &str); 8] = [
    ("obsidian", "曜石 (Termius)"),
    ("midnight", "午夜"),
    ("graphite", "石墨"),
    ("cloud", "云白"),
    ("linen", "亚麻"),
    ("mist", "雾青"),
    ("auto", "跟随系统 (曜石/云白)"),
    ("auto-soft", "跟随系统柔和 (石墨/亚麻)"),
];

pub fn resolve_palette(choice: &str, system_dark: bool) -> Palette {
    let id = match choice {
        "auto" => {
            if system_dark {
                "obsidian"
            } else {
                "cloud"
            }
        }
        "auto-soft" => {
            if system_dark {
                "graphite"
            } else {
                "linen"
            }
        }
        other => other,
    };
    PALETTES
        .iter()
        .copied()
        .find(|p| p.id == id)
        .unwrap_or(PALETTES[0])
}

pub fn apply_palette(ctx: &egui::Context, p: &Palette) {
    let mut visuals = if p.dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    visuals.panel_fill = p.bg;
    visuals.window_fill = p.bg_elev;
    visuals.extreme_bg_color = p.surface2;
    visuals.faint_bg_color = p.panel;
    visuals.code_bg_color = p.surface2;
    visuals.override_text_color = Some(p.text);
    visuals.window_stroke = Stroke::new(1.0_f32, p.border);
    visuals.window_corner_radius = CornerRadius::same(10);

    visuals.widgets.noninteractive.bg_fill = p.panel;
    visuals.widgets.noninteractive.weak_bg_fill = p.panel;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.text);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(7);

    visuals.widgets.inactive.bg_fill = p.surface3;
    visuals.widgets.inactive.weak_bg_fill = p.surface3;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, p.text);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(7);

    visuals.widgets.hovered.bg_fill = p.accent_soft;
    visuals.widgets.hovered.weak_bg_fill = p.accent_soft;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, p.accent);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, p.text);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(7);

    visuals.widgets.active.bg_fill = p.accent;
    visuals.widgets.active.weak_bg_fill = p.accent;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    visuals.widgets.active.corner_radius = CornerRadius::same(7);

    visuals.selection.bg_fill = p.accent_soft;
    visuals.selection.stroke = Stroke::new(1.0_f32, p.accent);

    ctx.all_styles_mut(|style| {
        style.visuals = visuals.clone();
    });
}
