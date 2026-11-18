//! The Claritas palette and egui visuals. Copied from `claritas-ui::theme` while that
//! crate is unpublished; keep the two in sync by hand (ADR 0002). Colours the views
//! do not use yet are added when they do.
use egui::{Color32, CornerRadius};

pub const BACKGROUND: Color32 = Color32::from_rgb(0x14, 0x16, 0x1a);
pub const PANEL: Color32 = Color32::from_rgb(0x1d, 0x20, 0x26);
pub const TEXT: Color32 = Color32::from_rgb(0xd8, 0xdc, 0xe4);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x7a, 0x80, 0x8c);
pub const ACCENT: Color32 = Color32::from_rgb(0x4f, 0xc3, 0xf7);
pub const SELECTION: Color32 = Color32::from_rgb(0x22, 0x3c, 0x4e);

/// Panel edges and chip boxes.
pub const BORDER: Color32 = Color32::from_rgb(0x34, 0x3a, 0x46);
/// Edges inside a panel: slot cards, meter troughs.
pub const BORDER_SOFT: Color32 = Color32::from_rgb(0x24, 0x28, 0x2f);
pub const GREEN: Color32 = Color32::from_rgb(0x6c, 0xd4, 0x7a);
pub const RED: Color32 = Color32::from_rgb(0xe0, 0x4a, 0x4a);
pub const RED_SOFT: Color32 = Color32::from_rgb(0xff, 0x8a, 0x8a);
pub const RED_FILL: Color32 = Color32::from_rgb(0x3a, 0x1d, 0x1d);
pub const ORANGE: Color32 = Color32::from_rgb(0xe0, 0x9a, 0x3a);

pub const CORNER_RADIUS: CornerRadius = CornerRadius::same(3);
/// Seconds the pointer rests before a tooltip; egui's 0.5 felt slow.
const TOOLTIP_DELAY: f32 = 0.2;
const ITEM_PADDING: egui::Vec2 = egui::vec2(8.0, 3.0);

pub fn small_dim(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(12.5).color(TEXT_DIM)
}

pub fn apply(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BACKGROUND;
    visuals.window_fill = PANEL;
    visuals.override_text_color = Some(TEXT);
    // The text colour is overridden to light everywhere, so the highlight must stay dark.
    visuals.selection.bg_fill = SELECTION;
    visuals.selection.stroke = egui::Stroke::new(1.0, ACCENT);
    for w in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        w.corner_radius = CORNER_RADIUS;
        // Widgets keep their size on hover and press; nothing shifts under the pointer.
        w.expansion = 0.0;
        // egui shrinks a frame's margin by the state's stroke width; with the same width in
        // every state, a button whose stroke is overridden keeps its size on hover.
        w.bg_stroke.width = 1.0;
    }
    visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
    visuals.menu_corner_radius = CornerRadius::same(4);
    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|style| {
        style.spacing.button_padding = ITEM_PADDING;
        style.interaction.tooltip_delay = TOOLTIP_DELAY;
    });
}
