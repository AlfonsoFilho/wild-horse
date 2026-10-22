//! The Wild Horse theme set (`themes/wild-horse.json`, UX review §15.9):
//! "Wild Horse Dark" and "Wild Horse Light", following the system appearance
//! unless the user picks one with the Theme commands.

use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry};
use gpui_kit::*;

actions!(theme, [ThemeSystem, ThemeLight, ThemeDark]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Default)]
struct Preference(Appearance);

impl Global for Preference {}

/// Register the theme set and the Theme actions, and apply the appearance.
/// Call once at start-up, after `gpui_kit::init`.
pub fn init(cx: &mut App) {
    match ThemeRegistry::global_mut(cx).load_themes_from_str(include_str!("../themes/wild-horse.json")) {
        Ok(()) => {
            let themes = ThemeRegistry::global(cx).themes().clone();
            if let (Some(dark), Some(light)) = (themes.get("Wild Horse Dark"), themes.get("Wild Horse Light")) {
                let theme = Theme::global_mut(cx);
                theme.dark_theme = dark.clone();
                theme.light_theme = light.clone();
            }
        }
        Err(e) => eprintln!("wh-ide: the Wild Horse theme does not load ({e}); using the gpui-kit default"),
    }
    cx.set_global(Preference::default());
    apply(None, cx);
    cx.on_action(|_: &ThemeSystem, cx| set(Appearance::System, cx));
    cx.on_action(|_: &ThemeLight, cx| set(Appearance::Light, cx));
    cx.on_action(|_: &ThemeDark, cx| set(Appearance::Dark, cx));
}

pub fn appearance(cx: &App) -> Appearance {
    cx.try_global::<Preference>().map(|p| p.0).unwrap_or_default()
}

pub fn set(a: Appearance, cx: &mut App) {
    cx.set_global(Preference(a));
    apply(None, cx);
}

fn apply(window: Option<&mut Window>, cx: &mut App) {
    match appearance(cx) {
        Appearance::System => Theme::sync_system_appearance(window, cx),
        Appearance::Light => Theme::change(ThemeMode::Light, None, cx),
        Appearance::Dark => Theme::change(ThemeMode::Dark, None, cx),
    }
}

/// Follow the OS appearance in `window` while the preference is System.
/// Call once per window after it opens.
pub fn follow_system(window: &mut Window) {
    window
        .observe_window_appearance(|window, cx| {
            if appearance(cx) == Appearance::System {
                apply(Some(window), cx);
            }
        })
        .detach();
}
