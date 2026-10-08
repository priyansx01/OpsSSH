//! Application semantic colors, independent of widgets and server environment badges.
use gpui::{App, Global, Hsla, Subscription, Window, px, rgb};
use gpui_component::{Theme, ThemeMode};

/// Surfaces use neutral charcoal; ruby is reserved for the primary action and focus.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub canvas: u32,
    pub sidebar: u32,
    pub surface: u32,
    pub raised: u32,
    pub hover: u32,
    pub border: u32,
    pub text: u32,
    pub muted: u32,
    pub ruby: u32,
    pub ruby_hover: u32,
    pub ruby_pressed: u32,
    pub ruby_tint: u32,
    pub on_ruby: u32,
}
pub const DARK: Palette = Palette {
    canvas: 0x141416,
    sidebar: 0x19191c,
    surface: 0x202024,
    raised: 0x27272c,
    hover: 0x303036,
    border: 0x424249,
    text: 0xf4f4f5,
    muted: 0xaaaab4,
    ruby: 0xbc3150,
    ruby_hover: 0xce3a5b,
    ruby_pressed: 0xa32440,
    ruby_tint: 0x3c202a,
    on_ruby: 0xffffff,
};
pub const LIGHT: Palette = Palette {
    canvas: 0xf5f4f5,
    sidebar: 0xeeecef,
    surface: 0xffffff,
    raised: 0xf1eff2,
    hover: 0xe7e3e9,
    border: 0xc7c1cb,
    text: 0x201c24,
    muted: 0x625a69,
    ruby: 0xab2343,
    ruby_hover: 0x961c39,
    ruby_pressed: 0x80162f,
    ruby_tint: 0xfae6ec,
    on_ruby: 0xffffff,
};

#[derive(Debug, Default)]
struct Preference {
    system: bool,
    reduced_motion: bool,
}
impl Global for Preference {}
fn color(value: u32) -> Hsla {
    rgb(value).into()
}

pub fn palette(cx: &App) -> Palette {
    if Theme::global(cx).mode.is_dark() {
        DARK
    } else {
        LIGHT
    }
}

/// Initialize the component theme first. Unknown stored themes fall back to dark.
pub fn apply(theme: &str, window: &mut Window, cx: &mut App) {
    let reduced_motion = cx.has_global::<Preference>() && cx.global::<Preference>().reduced_motion;
    cx.set_global(Preference {
        system: theme == "system",
        reduced_motion,
    });
    match theme {
        "light" => Theme::change(ThemeMode::Light, Some(window), cx),
        "system" => Theme::sync_system_appearance(Some(window), cx),
        _ => Theme::change(ThemeMode::Dark, Some(window), cx),
    }
    let p = palette(cx);
    Theme::update(cx, |theme| {
        theme.radius = px(7.);
        theme.radius_lg = px(12.);
        theme.shadow = true;
        theme.focus_ring = true;
        let c = &mut theme.colors;
        c.background = color(p.canvas);
        c.foreground = color(p.text);
        c.success = color(if theme.mode.is_dark() {
            0x7bcba2
        } else {
            0x176e46
        });
        c.warning = color(if theme.mode.is_dark() {
            0xe4bc77
        } else {
            0x885805
        });
        c.border = color(p.border);
        c.primary = color(p.ruby);
        c.primary_hover = color(p.ruby_hover);
        c.primary_active = color(p.ruby_pressed);
        c.primary_foreground = color(p.on_ruby);
        c.button_primary = c.primary;
        c.button_primary_hover = c.primary_hover;
        c.button_primary_active = c.primary_active;
        c.button_primary_foreground = c.primary_foreground;
        c.accent = color(p.hover);
        c.accent_foreground = color(p.text);
        c.ring = color(p.ruby);
        c.caret = color(p.text);
        c.selection = color(p.ruby_tint);
        c.button = color(p.raised);
        c.button_hover = color(p.hover);
        c.button_active = color(p.hover);
        c.button_foreground = color(p.text);
        c.secondary = color(p.raised);
        c.secondary_hover = color(p.hover);
        c.secondary_active = color(p.hover);
        c.secondary_foreground = color(p.text);
        c.button_secondary = c.secondary;
        c.button_secondary_hover = c.secondary_hover;
        c.button_secondary_active = c.secondary_active;
        c.button_secondary_foreground = c.secondary_foreground;
        c.muted = color(p.raised);
        c.muted_foreground = color(p.muted);
        c.input = color(p.border);
        c.popover = color(p.surface);
        c.popover_foreground = color(p.text);
        c.group_box = color(p.surface);
        c.group_box_foreground = color(p.text);
        c.accordion = color(p.surface);
        c.sidebar = color(p.sidebar);
        c.sidebar_foreground = color(p.text);
        c.sidebar_border = color(p.border);
        c.sidebar_accent = color(p.ruby_tint);
        c.sidebar_accent_foreground = color(p.text);
        c.sidebar_primary = color(p.ruby);
        c.sidebar_primary_foreground = color(p.on_ruby);
        c.list = color(p.surface);
        c.list_hover = color(p.hover);
        c.list_active = color(p.ruby_tint);
        c.list_active_border = color(p.ruby);
        c.list_even = color(p.raised);
        c.list_head = color(p.raised);
        c.tab = color(p.sidebar);
        c.tab_bar = color(p.sidebar);
        c.tab_bar_segmented = color(p.raised);
        c.tab_active = color(p.surface);
        c.tab_foreground = color(p.muted);
        c.tab_active_foreground = color(p.text);
        c.table = color(p.surface);
        c.table_active = color(p.ruby_tint);
        c.table_active_border = color(p.ruby);
        c.link = color(if theme.mode.is_dark() {
            0xf096ad
        } else {
            0x971d3b
        });
        c.link_hover = c.primary_hover;
        c.link_active = c.primary_active;
        c.scrollbar = color(p.sidebar);
        c.scrollbar_thumb = color(p.border);
        c.scrollbar_thumb_hover = color(p.muted);
        c.progress_bar = color(p.ruby);
    });
    set_reduced_motion(reduced_motion, cx);
}

/// Subscribe once for the window and retain or detach the returned subscription.
/// Switching to a fixed theme makes subsequent OS appearance events no-ops.
pub fn observe_system(window: &Window) -> Subscription {
    window.observe_window_appearance(|window, cx| {
        if cx.has_global::<Preference>() && cx.global::<Preference>().system {
            apply("system", window, cx);
        }
    })
}

/// Component transition durations respect the preference; terminal output stays unanimated.
pub fn set_reduced_motion(reduced: bool, cx: &mut App) {
    if !cx.has_global::<Preference>() {
        cx.set_global(Preference::default());
    }
    cx.global_mut::<Preference>().reduced_motion = reduced;
    cx.set_reduce_motion(reduced);
    Theme::update(cx, |theme| {
        theme.motion = Default::default();
        theme.motion.duration_fast = std::time::Duration::from_millis(120);
        theme.motion.duration_normal = std::time::Duration::from_millis(190);
        theme.motion.duration_slow = std::time::Duration::from_millis(280);
        if reduced {
            theme.motion.duration_fast = std::time::Duration::ZERO;
            theme.motion.duration_normal = std::time::Duration::ZERO;
            theme.motion.duration_slow = std::time::Duration::ZERO;
            theme.motion.distance_short = gpui::rems(0.);
            theme.motion.distance_medium = gpui::rems(0.);
        }
    });
}

/// A short underdamped entrance; bounded properties clamp its overshoot.
pub fn spring_out(t: f32) -> f32 {
    if t <= 0. {
        return 0.;
    }
    if t >= 1. {
        return 1.;
    }
    1. - (1. - t).powi(3) * (10. * t).cos()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn luminance(value: u32) -> f64 {
        let channel = |shift: u32| {
            let c = ((value >> shift) & 255u32) as f64 / 255.;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }
    fn contrast(a: u32, b: u32) -> f64 {
        let a = luminance(a);
        let b = luminance(b);
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
    #[test]
    fn text_tokens_meet_normal_text_contrast() {
        for p in [DARK, LIGHT] {
            for background in [p.canvas, p.sidebar, p.surface, p.raised] {
                assert!(contrast(p.text, background) >= 4.5);
                assert!(contrast(p.muted, background) >= 4.5);
            }
            assert!(contrast(p.on_ruby, p.ruby) >= 4.5);
        }
    }
}
