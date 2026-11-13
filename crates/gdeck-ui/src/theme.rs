//! The dark and purple look, in one place.
//!
//! Every colour is named here rather than written inline at a call site, so
//! the palette can be read and changed as a unit.

use eframe::egui::{self, Color32, Stroke};

pub const BG: Color32 = Color32::from_rgb(0x0A, 0x07, 0x10);
pub const SURFACE: Color32 = Color32::from_rgb(0x14, 0x0E, 0x1E);
pub const SURFACE_2: Color32 = Color32::from_rgb(0x1C, 0x14, 0x29);
pub const SURFACE_3: Color32 = Color32::from_rgb(0x24, 0x1A, 0x34);
pub const LINE: Color32 = Color32::from_rgb(0x2C, 0x20, 0x40);
pub const FG: Color32 = Color32::from_rgb(0xEF, 0xE9, 0xF8);
pub const FG_2: Color32 = Color32::from_rgb(0xC9, 0xBE, 0xDE);
pub const MUTED: Color32 = Color32::from_rgb(0x8E, 0x80, 0xAC);
pub const ACCENT: Color32 = Color32::from_rgb(0xA8, 0x55, 0xF7);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x7C, 0x3A, 0xED);
pub const GOOD: Color32 = Color32::from_rgb(0x35, 0xD3, 0x9A);
pub const WARN: Color32 = Color32::from_rgb(0xFB, 0xBF, 0x24);
pub const CRIT: Color32 = Color32::from_rgb(0xFB, 0x71, 0x85);

/// Colour for an action type, matching the chips in the plan mockup.
pub fn action_color(kind: &str) -> Color32 {
    match kind {
        "launch" => Color32::from_rgb(0xC4, 0xB5, 0xFD),
        "open" => Color32::from_rgb(0x7D, 0xD3, 0xFC),
        "note" => Color32::from_rgb(0x6E, 0xE7, 0xB7),
        "shell" => Color32::from_rgb(0xFC, 0xD3, 0x4D),
        _ => MUTED,
    }
}

/// Load the faces the interface draws with.
///
/// egui rasterises glyphs into a texture atlas with grayscale coverage and no
/// hinting, while Windows draws its own text with ClearType and hinted
/// outlines. That gap lives in the renderer and no setting here closes it, so
/// the choice of face is where the difference is actually made.
///
/// Inter is embedded rather than read from the system because it is drawn to
/// render well unhinted, which is exactly the condition egui imposes. Segoe UI
/// assumes hinting and thins out without it. Embedding also means the
/// interface looks the same on a machine that has neither installed. Two
/// static weights rather than the one variable file, because the rasteriser
/// uses a variable font's default instance and cannot reach the semibold axis.
///
/// Inter is under the SIL Open Font License 1.1 and the licence travels with
/// it in `assets/Inter-LICENSE.txt`.
///
/// The monospace face is still read from the system. It carries key ids and
/// paths only, and embedding a third file for that would cost another 400 KB.
/// egui's bundled Hack stays behind it as the last resort.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    embed(
        &mut fonts,
        "inter",
        egui::FontFamily::Proportional,
        include_bytes!("../assets/Inter-Regular.ttf"),
    );

    // Seed the heavy family from the normal one first, so any glyph the
    // semibold face lacks still resolves behind it.
    let proportional = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    fonts.families.insert(strong(), proportional);
    embed(
        &mut fonts,
        "inter-strong",
        strong(),
        include_bytes!("../assets/Inter-SemiBold.ttf"),
    );

    load(
        &mut fonts,
        "mono",
        egui::FontFamily::Monospace,
        &[
            r"C:\Windows\Fonts\CascadiaMono.ttf",
            r"C:\Windows\Fonts\consola.ttf",
        ],
    );

    ctx.set_fonts(fonts);
}

/// Put a face compiled into the binary at the front of `family`.
fn embed(
    fonts: &mut egui::FontDefinitions,
    name: &str,
    family: egui::FontFamily,
    bytes: &'static [u8],
) {
    fonts
        .font_data
        .insert(name.to_owned(), egui::FontData::from_static(bytes).into());
    fonts
        .families
        .entry(family)
        .or_default()
        .insert(0, name.to_owned());
}

/// The heavier face, for small text where thin stems break up.
pub fn strong() -> egui::FontFamily {
    egui::FontFamily::Name("strong".into())
}

/// Put the first readable candidate at the front of `family`, leaving the
/// bundled fonts behind it to cover glyphs the chosen face lacks.
fn load(
    fonts: &mut egui::FontDefinitions,
    name: &str,
    family: egui::FontFamily,
    candidates: &[&str],
) {
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert(name.to_owned(), egui::FontData::from_owned(bytes).into());
            fonts
                .families
                .entry(family)
                .or_default()
                .insert(0, name.to_owned());
            return;
        }
    }
}

pub fn apply(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();

    visuals.override_text_color = Some(FG);
    visuals.panel_fill = BG;
    visuals.window_fill = SURFACE;
    visuals.faint_bg_color = SURFACE_2;
    visuals.extreme_bg_color = SURFACE_2;
    visuals.selection.bg_fill = ACCENT_DIM;
    visuals.selection.stroke = Stroke::new(1.0, FG);
    visuals.hyperlink_color = ACCENT;
    visuals.window_stroke = Stroke::new(1.0, LINE);

    visuals.widgets.noninteractive.bg_fill = SURFACE;
    visuals.widgets.noninteractive.weak_bg_fill = SURFACE;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, FG_2);

    visuals.widgets.inactive.bg_fill = SURFACE_2;
    visuals.widgets.inactive.weak_bg_fill = SURFACE_2;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, FG_2);

    visuals.widgets.hovered.bg_fill = SURFACE_3;
    visuals.widgets.hovered.weak_bg_fill = SURFACE_3;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_DIM);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, FG);

    visuals.widgets.active.bg_fill = SURFACE_3;
    visuals.widgets.active.weak_bg_fill = SURFACE_3;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, FG);

    ctx.set_visuals(visuals);
}
