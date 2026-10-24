//! Look and feel: colours, fonts, and the few widgets drawn by hand (buttons, progress bar,
//! cards, the brick logo).

use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Frame, Margin, Pos2, Rect,
    Response, RichText, Sense, Shadow, Stroke, StrokeKind, TextStyle, Ui, Vec2, WidgetInfo, epaint::Mesh,
    pos2, vec2,
};

/// Phosphor icon glyphs (MIT; `assets/fonts/Phosphor.ttf` holds only these).
pub mod icon {
    pub const PLAY: &str = "\u{e3d0}";
    pub const DOWNLOAD: &str = "\u{e20c}";
    pub const UPDATE: &str = "\u{e094}";
    pub const SHIELD_CHECK: &str = "\u{e40c}";
    pub const WRENCH: &str = "\u{e5d4}";
    pub const X: &str = "\u{e4f6}";
    pub const GEAR: &str = "\u{e272}";
    pub const FOLDER: &str = "\u{e256}";
    pub const SERVER: &str = "\u{e2a0}";
    pub const GLOBE: &str = "\u{e288}";
    pub const USER: &str = "\u{e4c2}";
    pub const CHECK_CIRCLE: &str = "\u{e184}";
    pub const WARNING: &str = "\u{e4e0}";
    pub const WARNING_CIRCLE: &str = "\u{e4e2}";
    pub const KEY: &str = "\u{e2d6}";
    pub const CARET_DOWN: &str = "\u{e136}";
    pub const CARET_UP: &str = "\u{e13c}";
    pub const LIST: &str = "\u{e2f2}";
    pub const CLOUD_DOWN: &str = "\u{e1ac}";
    pub const PLUGS: &str = "\u{eb56}";
    pub const HOURGLASS: &str = "\u{e2b8}";
    pub const LINUX: &str = "\u{eb02}";
    pub const WINDOWS: &str = "\u{e692}";
    pub const PULSE: &str = "\u{e000}";
    pub const CUBE: &str = "\u{e1da}";
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub const BG: Color32 = rgb(0x0e, 0x11, 0x18);
pub const CARD: Color32 = rgb(0x16, 0x1a, 0x24);
pub const CARD_STROKE: Color32 = rgb(0x24, 0x2a, 0x38);
pub const FIELD: Color32 = rgb(0x0b, 0x0e, 0x14);
pub const TEXT: Color32 = rgb(0xe8, 0xeb, 0xf2);
pub const WEAK: Color32 = rgb(0x8a, 0x93, 0xa8);
/// LEGO yellow, red and blue.
pub const ACCENT: Color32 = rgb(0xff, 0xc8, 0x1f);
pub const ACCENT_TEXT: Color32 = rgb(0x1c, 0x15, 0x02);
pub const RED: Color32 = rgb(0xef, 0x4b, 0x52);
pub const BLUE: Color32 = rgb(0x3b, 0x8c, 0xf7);
pub const GREEN: Color32 = rgb(0x36, 0xc7, 0x7c);
pub const AMBER: Color32 = rgb(0xf5, 0xa5, 0x24);

pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

pub fn display() -> FontFamily {
    FontFamily::Name("display".into())
}

/// Fonts and style for the whole window (always dark, like most game launchers).
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, data: &'static [u8], tweak: egui::FontTweak| {
        fonts.font_data.insert(name.into(), Arc::new(FontData::from_static(data).tweak(tweak)));
    };
    add("inter", include_bytes!("../assets/fonts/Inter-Regular.ttf"), Default::default());
    add("inter-semibold", include_bytes!("../assets/fonts/Inter-SemiBold.ttf"), Default::default());
    add("inter-display", include_bytes!("../assets/fonts/InterDisplay-Bold.ttf"), Default::default());
    add(
        "phosphor",
        include_bytes!("../assets/fonts/Phosphor.ttf"),
        egui::FontTweak { scale: 1.15, y_offset_factor: 0.08, ..Default::default() },
    );
    // egui's own fonts stay as fallbacks for anything Inter lacks.
    let fallback = fonts.families[&FontFamily::Proportional].clone();
    let family = |first: &str| [first.to_string(), "phosphor".into()].into_iter().chain(fallback.iter().cloned()).collect();
    fonts.families.insert(FontFamily::Proportional, family("inter"));
    fonts.families.insert(semibold(), family("inter-semibold"));
    fonts.families.insert(display(), family("inter-display"));
    fonts.families.get_mut(&FontFamily::Monospace).unwrap().push("phosphor".into());
    ctx.set_fonts(fonts);

    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Heading, FontId::new(22.0, display())),
            (TextStyle::Monospace, FontId::monospace(12.5)),
        ]
        .into();
        let s = &mut style.spacing;
        s.item_spacing = vec2(8.0, 8.0);
        s.button_padding = vec2(12.0, 6.0);
        s.interact_size.y = 30.0;
        s.slider_width = 180.0;

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.panel_fill = BG;
        v.window_fill = CARD;
        v.window_stroke = Stroke::new(1.0, CARD_STROKE);
        v.window_corner_radius = CornerRadius::same(16);
        v.window_shadow = Shadow { offset: [0, 8], blur: 32, spread: 0, color: Color32::from_black_alpha(140) };
        v.popup_shadow = Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(120) };
        v.menu_corner_radius = CornerRadius::same(10);
        v.extreme_bg_color = FIELD;
        v.text_edit_bg_color = Some(FIELD);
        v.faint_bg_color = CARD;
        v.code_bg_color = FIELD;
        v.weak_text_color = Some(WEAK);
        v.hyperlink_color = BLUE;
        v.warn_fg_color = AMBER;
        v.error_fg_color = RED;
        v.selection.bg_fill = BLUE.gamma_multiply(0.45);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.slider_trailing_fill = true;

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = CARD;
        w.noninteractive.weak_bg_fill = CARD;
        w.noninteractive.bg_stroke = Stroke::new(1.0, CARD_STROKE);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        for (wv, fill, stroke) in [
            (&mut w.inactive, rgb(0x22, 0x28, 0x35), rgb(0x2c, 0x33, 0x43)),
            (&mut w.hovered, rgb(0x2a, 0x31, 0x40), rgb(0x3d, 0x46, 0x5b)),
            (&mut w.active, rgb(0x33, 0x3b, 0x4d), rgb(0x4a, 0x54, 0x6b)),
            (&mut w.open, rgb(0x2a, 0x31, 0x40), rgb(0x3d, 0x46, 0x5b)),
        ] {
            wv.bg_fill = fill;
            wv.weak_bg_fill = fill;
            wv.bg_stroke = Stroke::new(1.0, stroke);
            wv.fg_stroke = Stroke::new(1.0, TEXT);
            wv.corner_radius = CornerRadius::same(8);
            wv.expansion = 0.0;
        }
    });
}

/// The window icon: the brick logo.
pub fn window_icon() -> egui::IconData {
    const N: usize = 64;
    // 4×4 supersampling of the same shapes `brick` paints.
    let mut rgba = vec![0u8; N * N * 4];
    for y in 0..N {
        for x in 0..N {
            let (mut body, mut stud) = (0u32, 0u32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let p = pos2((x as f32 + (sx as f32 + 0.5) / 4.0) / N as f32, (y as f32 + (sy as f32 + 0.5) / 4.0) / N as f32);
                    match brick_shape(p) {
                        Some(true) => stud += 1,
                        Some(false) => body += 1,
                        None => {}
                    }
                }
            }
            let a = (body + stud) as f32 / 16.0;
            let t = if body + stud == 0 { 0.0 } else { stud as f32 / (body + stud) as f32 };
            let (dark, light) = (ACCENT, rgb(0xff, 0xe0, 0x7a));
            let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
            let i = (y * N + x) * 4;
            rgba[i..i + 4].copy_from_slice(&[mix(dark.r(), light.r()), mix(dark.g(), light.g()), mix(dark.b(), light.b()), (a * 255.0) as u8]);
        }
    }
    egui::IconData { rgba, width: N as u32, height: N as u32 }
}

/// In the unit square: `Some(true)` on a stud, `Some(false)` on the body, `None` outside.
fn brick_shape(p: Pos2) -> Option<bool> {
    let body = Rect::from_min_max(pos2(0.06, 0.06), pos2(0.94, 0.94));
    let r = 0.18;
    let inner = body.shrink(r);
    let d = vec2((p.x - p.x.clamp(inner.left(), inner.right())).abs(), (p.y - p.y.clamp(inner.top(), inner.bottom())).abs());
    if d.length() > r {
        return None;
    }
    let on_stud = [(0.32, 0.32), (0.68, 0.32), (0.32, 0.68), (0.68, 0.68)]
        .iter()
        .any(|&(cx, cy)| pos2(cx, cy).distance(p) < 0.13);
    Some(on_stud)
}

/// The brick logo in `rect` (square).
pub fn brick(painter: &egui::Painter, rect: Rect) {
    let s = rect.width();
    let body = Rect::from_min_max(rect.min + Vec2::splat(0.06 * s), rect.max - Vec2::splat(0.06 * s));
    painter.rect_filled(body.translate(vec2(0.0, 0.04 * s)), 0.18 * s, Color32::from_black_alpha(90));
    painter.rect_filled(body, 0.18 * s, ACCENT);
    for (cx, cy) in [(0.32, 0.32), (0.68, 0.32), (0.32, 0.68), (0.68, 0.68)] {
        let c = rect.min + vec2(cx, cy) * s;
        painter.circle_filled(c + vec2(0.0, 0.025 * s), 0.13 * s, rgb(0xd9, 0xa3, 0x00));
        painter.circle_filled(c, 0.13 * s, rgb(0xff, 0xdb, 0x5c));
    }
}

/// A horizontal gradient.
pub fn gradient(painter: &egui::Painter, rect: Rect, left: Color32, right: Color32) {
    let mut mesh = Mesh::default();
    for (pos, color) in
        [(rect.left_top(), left), (rect.right_top(), right), (rect.right_bottom(), right), (rect.left_bottom(), left)]
    {
        mesh.colored_vertex(pos, color);
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(mesh);
}

/// A rounded panel holding one part of the window.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, CARD_STROKE))
        .corner_radius(14)
        .inner_margin(18)
        .shadow(Shadow { offset: [0, 4], blur: 18, spread: 0, color: Color32::from_black_alpha(70) })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            add(ui)
        })
        .inner
}

/// The small caps title at the top of a card.
pub fn card_title(ui: &mut Ui, icon: &str, title: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(RichText::new(icon).color(ACCENT).size(15.0));
        ui.label(RichText::new(title.to_uppercase()).family(semibold()).size(11.5).color(WEAK).extra_letter_spacing(1.2));
    });
    ui.add_space(4.0);
}

/// A label above a field.
pub fn field_label(ui: &mut Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(RichText::new(text).family(semibold()).size(12.5).color(WEAK));
}

pub fn text_field(text: &mut String) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(text).margin(Margin::symmetric(10, 7))
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Primary,
    Secondary,
    Danger,
}

/// A large button with an icon, drawn by hand so it can be coloured per kind with hover and
/// press states (egui's `Button::fill` overrides those).
pub fn big_button(ui: &mut Ui, kind: Kind, icon: &str, text: &str, enabled: bool, size: Vec2) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, enabled, text));
    let resp = if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    if ui.is_rect_visible(rect) {
        let (fill, fg, stroke) = match kind {
            Kind::Primary => (ACCENT, ACCENT_TEXT, Color32::TRANSPARENT),
            Kind::Secondary => (rgb(0x24, 0x2a, 0x38), TEXT, rgb(0x33, 0x3b, 0x4d)),
            Kind::Danger => (rgb(0x3a, 0x1c, 0x22), rgb(0xff, 0x9a, 0x9e), rgb(0x5c, 0x26, 0x2d)),
        };
        let fill = if !enabled {
            fill.gamma_multiply(0.35)
        } else if resp.is_pointer_button_down_on() {
            lerp(fill, Color32::BLACK, 0.12)
        } else if resp.hovered() {
            lerp(fill, Color32::WHITE, 0.1)
        } else {
            fill
        };
        let fg = if enabled { fg } else { fg.gamma_multiply(0.45) };
        let painter = ui.painter();
        if kind == Kind::Primary && enabled {
            painter.rect_filled(rect.translate(vec2(0.0, 3.0)), 10, rgb(0xb8, 0x88, 0x00));
        }
        painter.rect(rect, 10, fill, Stroke::new(1.0, stroke), StrokeKind::Inside);
        focus_ring(ui, &resp, rect, 10);
        let font = FontId::new(if size.y >= 44.0 { 16.0 } else { 14.0 }, semibold());
        let icon_g = painter.layout_no_wrap(icon.into(), font.clone(), fg);
        let text_g = painter.layout_no_wrap(text.into(), font, fg);
        let gap = if icon.is_empty() { 0.0 } else { 8.0 };
        let w = icon_g.size().x + gap + text_g.size().x;
        let x = rect.center().x - w / 2.0;
        painter.galley(pos2(x, rect.center().y - icon_g.size().y / 2.0), icon_g.clone(), fg);
        painter.galley(pos2(x + icon_g.size().x + gap, rect.center().y - text_g.size().y / 2.0), text_g, fg);
    }
    resp
}

/// Keyboard focus, for the hand-drawn widgets.
fn focus_ring(ui: &Ui, resp: &Response, rect: Rect, radius: u8) {
    if resp.has_focus() {
        ui.painter().rect_stroke(rect.expand(2.0), radius + 2, Stroke::new(2.0, BLUE), StrokeKind::Outside);
    }
}

/// A small square button with just an icon.
pub fn icon_button(ui: &mut Ui, icon: &str, tooltip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, tooltip));
    if ui.is_rect_visible(rect) {
        let fill = if resp.hovered() { Color32::from_white_alpha(22) } else { Color32::from_white_alpha(8) };
        ui.painter().rect_filled(rect, 9, fill);
        focus_ring(ui, &resp, rect, 9);
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, FontId::proportional(18.0), TEXT);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tooltip)
}

/// A thin rounded progress bar; `None` slides a segment back and forth.
pub fn progress(ui: &mut Ui, fraction: Option<f32>, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 4, FIELD);
    let fill = match fraction {
        Some(f) => {
            let f = f.clamp(0.0, 1.0);
            (f > 0.0).then(|| Rect::from_min_size(rect.min, vec2((rect.width() * f).max(8.0), rect.height())))
        }
        None => {
            let t = ui.input(|i| i.time) as f32;
            let phase = (t * 0.8).fract();
            let w = rect.width() * 0.3;
            let x = rect.left() - w + phase * (rect.width() + w);
            ui.ctx().request_repaint();
            Some(Rect::from_min_size(pos2(x, rect.top()), vec2(w, rect.height())).intersect(rect))
        }
    };
    if let Some(fill) = fill.filter(|r| r.width() > 0.0) {
        painter.rect_filled(fill, 4, color);
        // a soft highlight along the top
        let top = Rect::from_min_max(fill.min + vec2(2.0, 1.0), pos2(fill.right() - 2.0, fill.top() + 3.0));
        if top.width() > 0.0 {
            painter.rect_filled(top, 2, Color32::from_white_alpha(45));
        }
    }
}

/// A large icon in a tinted circle, for the status headline.
pub fn badge(ui: &mut Ui, icon: &str, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(52.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 26.0, color.gamma_multiply(0.16));
    ui.painter().circle_stroke(rect.center(), 25.5, Stroke::new(1.0, color.gamma_multiply(0.35)));
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, FontId::proportional(24.0), color);
}

/// A pill with a coloured dot.
pub fn chip(ui: &mut Ui, color: Color32, text: &str) -> Response {
    let galley = ui.painter().layout_no_wrap(text.into(), FontId::new(12.5, semibold()), TEXT);
    let size = vec2(galley.size().x + 32.0, 26.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect(rect, 13, Color32::from_black_alpha(70), Stroke::new(1.0, Color32::from_white_alpha(20)), StrokeKind::Inside);
    painter.circle_filled(pos2(rect.left() + 14.0, rect.center().y), 4.0, color);
    painter.circle_filled(pos2(rect.left() + 14.0, rect.center().y), 7.0, color.gamma_multiply(0.25));
    painter.galley(pos2(rect.left() + 24.0, rect.center().y - galley.size().y / 2.0), galley, TEXT);
    resp
}

/// Buttons side by side, one of them selected. Returns whether the selection changed.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str, &str)], enabled: bool) -> bool {
    let before = *value;
    let width = (ui.available_width() / options.len() as f32).min(150.0);
    let (outer, _) = ui.allocate_exact_size(vec2(width * options.len() as f32, 34.0), Sense::hover());
    ui.painter().rect(outer, 9, FIELD, Stroke::new(1.0, CARD_STROKE), StrokeKind::Inside);
    for (i, (v, icon, label)) in options.iter().enumerate() {
        let rect = Rect::from_min_size(outer.min + vec2(width * i as f32, 0.0), vec2(width, outer.height())).shrink(3.0);
        let resp = ui.interact(rect, ui.scope_id().with(("seg", i)), if enabled { Sense::click() } else { Sense::hover() });
        let selected = *v == *value;
        resp.widget_info(|| WidgetInfo::selected(egui::Role::RadioButton, enabled, selected, *label));
        if resp.clicked() {
            *value = *v;
        }
        let fill = if selected {
            rgb(0x2a, 0x31, 0x40)
        } else if resp.hovered() && enabled {
            Color32::from_white_alpha(8)
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, 7, fill);
        focus_ring(ui, &resp, rect, 7);
        let color = if selected { TEXT } else { WEAK };
        let color = if enabled { color } else { color.gamma_multiply(0.5) };
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, format!("{icon}  {label}"), FontId::new(13.5, semibold()), color);
    }
    *value != before
}

/// A tinted box with an icon and wrapped text (warnings, notes).
pub fn callout(ui: &mut Ui, icon: &str, color: Color32, text: &str) {
    Frame::new().fill(color.gamma_multiply(0.1)).stroke(Stroke::new(1.0, color.gamma_multiply(0.3))).corner_radius(10).inner_margin(Margin::symmetric(12, 9)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_top(|ui| {
            ui.label(RichText::new(icon).color(color).size(16.0));
            ui.add(egui::Label::new(RichText::new(text).color(lerp(color, TEXT, 0.55))).wrap());
        });
    });
}

pub fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()), m(a.a(), b.a()))
}
