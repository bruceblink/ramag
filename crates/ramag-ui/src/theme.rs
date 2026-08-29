//! VSCode 风格暗 / 亮主题。`init_theme` 启动时初始化，ActivityBar 主题按钮两态切换（浅 / 暗）

use std::sync::Arc;

use gpui::{App, Global, Hsla, hsla};
use gpui_component::{Theme, ThemeMode, highlighter::HighlightTheme};
use ramag_domain::traits::Storage;

/// 让 UI 层切主题时访问 Storage 做持久化
pub struct StorageGlobal(pub Arc<dyn Storage>);
impl Global for StorageGlobal {}

/// main 可能没注入，None 时不持久化
pub fn storage_from_cx(cx: &App) -> Option<Arc<dyn Storage>> {
    cx.try_global::<StorageGlobal>().map(|g| g.0.clone())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
}

/// preference "dark" 用暗色；其余（缺省 / 旧版 "system" 残值）一律默认浅色
pub fn init_theme(preference: Option<&str>, cx: &mut App) {
    let mode = match preference {
        Some("dark") => Mode::Dark,
        _ => Mode::Light,
    };
    apply_theme(mode, cx);
}

pub fn apply_theme(mode: Mode, cx: &mut App) {
    match mode {
        Mode::Dark => {
            Theme::change(ThemeMode::Dark, None, cx);
            apply_dark_palette(Theme::global_mut(cx));
        }
        Mode::Light => {
            Theme::change(ThemeMode::Light, None, cx);
            apply_light_palette(Theme::global_mut(cx));
        }
    }
    // 命令编辑器背景对齐主背景：gpui 默认 editor.background 是纯黑，浮在主背景上显突兀
    normalize_editor_highlight_theme(mode, cx);
    // 结果表和其它需要明确可发现性的内容区域会自行挂载滚动条；统一恢复可见轨道和滑块。
    configure_scrollbar_paint(Theme::global_mut(cx));
}

/// 切换浅色 / 深色主题，立即刷新全部窗口并把最终选择异步写入 Storage。
pub fn toggle_theme(cx: &mut App) {
    let next = match current_mode(cx) {
        Mode::Light => Mode::Dark,
        Mode::Dark => Mode::Light,
    };
    apply_theme(next, cx);
    cx.refresh_windows();
    let preference = match next {
        Mode::Dark => "dark",
        Mode::Light => "light",
    };
    crate::preferences::persist_preference_latest("theme_mode", preference.to_string(), cx);
}

/// 为可拖拽滚动条提供稳定的对比度，避免主题配置中的透明色让滚动条只剩滚动状态而不可见。
fn configure_scrollbar_paint(theme: &mut Theme) {
    let mut track = theme.border;
    track.a = 0.45;
    let mut thumb = theme.muted_foreground;
    thumb.a = 0.68;
    let mut thumb_hover = theme.foreground;
    thumb_hover.a = 0.82;
    theme.scrollbar = track;
    theme.scrollbar_thumb = thumb;
    theme.scrollbar_thumb_hover = thumb_hover;
}

/// 编辑器背景对齐主背景；浅色默认主题的亮蓝色注释改为更常见的绿色。
fn normalize_editor_highlight_theme(mode: Mode, cx: &mut App) {
    let theme = Theme::global_mut(cx);
    let bg = theme.background;
    let hl = normalized_editor_highlight_theme(mode, bg, &theme.highlight_theme);
    theme.highlight_theme = Arc::new(hl);
}

fn normalized_editor_highlight_theme(
    mode: Mode,
    bg: Hsla,
    source: &HighlightTheme,
) -> HighlightTheme {
    let mut hl = source.clone();
    hl.style.editor_background = Some(bg);
    if matches!(mode, Mode::Light) {
        let comment = hl.style.syntax.string;
        hl.style.syntax.comment = comment;
        hl.style.syntax.comment_doc = comment;
    }
    hl
}

pub fn current_mode(cx: &App) -> Mode {
    if matches!(Theme::global(cx).mode, ThemeMode::Light) {
        Mode::Light
    } else {
        Mode::Dark
    }
}

/// VSCode Dark+ 配色
fn apply_dark_palette(theme: &mut Theme) {
    // VSCode 蓝（#007ACC）
    let accent = hsl(207.0, 100.0, 42.0);
    let accent_hover = hsl(207.0, 100.0, 50.0);
    let accent_active = hsl(207.0, 100.0, 36.0);

    theme.accent = accent;
    theme.accent_foreground = hsl(0.0, 0.0, 100.0);
    theme.primary = accent;
    theme.primary_hover = accent_hover;
    theme.primary_active = accent_active;
    theme.primary_foreground = hsl(0.0, 0.0, 100.0);

    theme.link = accent_hover;
    theme.link_hover = hsl(207.0, 100.0, 60.0);
    theme.link_active = accent_active;

    theme.background = hsl(0.0, 0.0, 12.0); // #1E1E1E
    theme.secondary = hsl(0.0, 0.0, 15.0); // #252526
    theme.sidebar = hsl(0.0, 0.0, 15.0);
    theme.title_bar = hsl(0.0, 0.0, 19.0);
    theme.title_bar_border = hsl(0.0, 0.0, 25.0);

    theme.border = hsl(0.0, 0.0, 25.0);
    theme.input = hsl(0.0, 0.0, 18.0);

    theme.foreground = hsl(0.0, 0.0, 80.0);
    theme.muted = hsl(0.0, 0.0, 22.0);
    theme.muted_foreground = hsl(0.0, 0.0, 55.0);
    theme.secondary_foreground = hsl(0.0, 0.0, 80.0);

    theme.danger = hsl(0.0, 75.0, 55.0);
    theme.danger_hover = hsl(0.0, 75.0, 60.0);
    theme.danger_active = hsl(0.0, 75.0, 48.0);
    theme.danger_foreground = hsl(0.0, 0.0, 100.0);

    theme.success = hsl(120.0, 50.0, 45.0);
    theme.success_hover = hsl(120.0, 50.0, 52.0);
    theme.success_active = hsl(120.0, 50.0, 38.0);
    theme.success_foreground = hsl(0.0, 0.0, 100.0);

    theme.info = accent;
    theme.info_hover = accent_hover;
    theme.info_active = accent_active;
    theme.info_foreground = hsl(0.0, 0.0, 100.0);

    theme.selection = accent.opacity(0.35);

    // 列表/菜单选中与悬停：暗色下用稍浓的淡化 accent（深底上要看得出），
    // 配普通 foreground 文字仍可读；不要实色 accent 压住文字
    theme.list_active = accent.opacity(0.24);
    theme.list_active_border = accent.opacity(0.45);
    theme.list_hover = accent.opacity(0.12);

    theme.popover = hsl(0.0, 0.0, 17.0);
    theme.popover_foreground = hsl(0.0, 0.0, 86.0);

    // 补全前缀高亮：暗色下浅蓝可见于选中态深蓝 bg
    theme.blue = hsl(207.0, 90.0, 70.0);
    theme.blue_light = hsl(207.0, 90.0, 80.0);
}

/// VSCode Light+ 配色
fn apply_light_palette(theme: &mut Theme) {
    let accent = hsl(207.0, 100.0, 38.0);
    let accent_hover = hsl(207.0, 100.0, 32.0);
    let accent_active = hsl(207.0, 100.0, 28.0);

    theme.accent = accent;
    theme.accent_foreground = hsl(0.0, 0.0, 100.0);
    theme.primary = accent;
    theme.primary_hover = accent_hover;
    theme.primary_active = accent_active;
    theme.primary_foreground = hsl(0.0, 0.0, 100.0);

    theme.link = accent;
    theme.link_hover = accent_hover;
    theme.link_active = accent_active;

    theme.background = hsl(0.0, 0.0, 100.0); // #FFFFFF
    theme.secondary = hsl(0.0, 0.0, 96.0); // #F3F3F3
    theme.sidebar = hsl(0.0, 0.0, 96.0);
    theme.title_bar = hsl(0.0, 0.0, 92.0);
    theme.title_bar_border = hsl(0.0, 0.0, 82.0);

    theme.border = hsl(0.0, 0.0, 85.0);
    theme.input = hsl(0.0, 0.0, 100.0);

    theme.foreground = hsl(0.0, 0.0, 12.0);
    theme.muted = hsl(0.0, 0.0, 92.0);
    theme.muted_foreground = hsl(0.0, 0.0, 38.0);
    theme.secondary_foreground = hsl(0.0, 0.0, 12.0);

    theme.danger = hsl(0.0, 65.0, 48.0);
    theme.danger_hover = hsl(0.0, 65.0, 42.0);
    theme.danger_active = hsl(0.0, 65.0, 36.0);
    theme.danger_foreground = hsl(0.0, 0.0, 100.0);

    theme.success = hsl(120.0, 45.0, 35.0);
    theme.success_hover = hsl(120.0, 45.0, 30.0);
    theme.success_active = hsl(120.0, 45.0, 26.0);
    theme.success_foreground = hsl(0.0, 0.0, 100.0);

    theme.info = accent;
    theme.info_hover = accent_hover;
    theme.info_active = accent_active;
    theme.info_foreground = hsl(0.0, 0.0, 100.0);

    theme.selection = accent.opacity(0.20);

    // 列表/菜单选中与悬停：亮色下用更淡的 accent（白底上淡蓝），保证暗字可读
    theme.list_active = accent.opacity(0.14);
    theme.list_active_border = accent.opacity(0.30);
    theme.list_hover = accent.opacity(0.07);

    theme.popover = hsl(0.0, 0.0, 100.0);
    theme.popover_foreground = hsl(0.0, 0.0, 12.0);

    // 补全前缀高亮：浅色下 blue 须比 accent 更亮才能看清
    theme.blue = hsl(207.0, 100.0, 65.0);
    theme.blue_light = hsl(207.0, 100.0, 75.0);
}

/// 将 HSL 数值范围转换为 Hsla。
fn hsl(h: f32, s: f32, l: f32) -> Hsla {
    hsla(h / 360.0, s / 100.0, l / 100.0, 1.0)
}

trait Opacity {
    fn opacity(self, alpha: f32) -> Self;
}

impl Opacity for Hsla {
    fn opacity(mut self, alpha: f32) -> Self {
        self.a = alpha.clamp(0.0, 1.0);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_editor_comments_are_readable_and_background_matches_app() {
        let source = HighlightTheme::default_light();
        let bg = hsl(0.0, 0.0, 100.0);

        let normalized = normalized_editor_highlight_theme(Mode::Light, bg, &source);

        assert_eq!(normalized.style.editor_background, Some(bg));
        assert_eq!(
            normalized.style.syntax.comment,
            normalized.style.syntax.string
        );
        assert_eq!(
            normalized.style.syntax.comment_doc,
            normalized.style.syntax.string
        );
    }

    #[test]
    fn dark_editor_keeps_existing_comment_style() {
        let source = HighlightTheme::default_dark();
        let normalized =
            normalized_editor_highlight_theme(Mode::Dark, hsl(0.0, 0.0, 12.0), &source);

        assert_eq!(normalized.style.syntax.comment, source.style.syntax.comment);
        assert_eq!(
            normalized.style.syntax.comment_doc,
            source.style.syntax.comment_doc
        );
    }
}
