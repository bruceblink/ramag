//! 跨显示器窗口布局：选择前台应用所在屏，并约束抽屉不超出工作区。

use std::rc::Rc;

use gpui::{App, Bounds, Pixels, PlatformDisplay, point, px, size};

const DRAWER_HEIGHT: f32 = 280.0;
const DRAWER_MARGIN: f32 = 5.0;
const MIN_EXTENT: f32 = 1.0;

pub(crate) fn preferred_display(
    cx: &App,
    preferred_index: Option<usize>,
) -> Option<Rc<dyn PlatformDisplay>> {
    let displays = cx.displays();
    preferred_index
        .and_then(|index| displays.get(index).cloned())
        .or_else(|| cx.primary_display())
        .or_else(|| displays.into_iter().next())
}

pub(crate) fn drawer_bounds(visible: Bounds<Pixels>) -> Bounds<Pixels> {
    let screen_width = visible.size.width.to_f64() as f32;
    let screen_height = visible.size.height.to_f64() as f32;
    let margin_x = safe_margin(screen_width);
    let margin_y = safe_margin(screen_height);
    let width = (screen_width - margin_x * 2.0).max(MIN_EXTENT);
    let height = DRAWER_HEIGHT.min((screen_height - margin_y * 2.0).max(MIN_EXTENT));
    let x = visible.origin.x.to_f64() as f32 + margin_x;
    let y = visible.origin.y.to_f64() as f32 + screen_height - margin_y - height;
    Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
}

fn safe_margin(extent: f32) -> f32 {
    DRAWER_MARGIN.min(((extent - MIN_EXTENT) / 2.0).max(0.0))
}

#[cfg(test)]
mod tests {
    use gpui::{Bounds, point, px, size};

    use super::drawer_bounds;

    #[test]
    fn drawer_uses_bottom_of_visible_work_area() {
        let bounds = drawer_bounds(Bounds::new(
            point(px(-1920.0), px(0.0)),
            size(px(1920.0), px(1040.0)),
        ));
        assert_eq!(bounds.origin, point(px(-1915.0), px(755.0)));
        assert_eq!(bounds.size, size(px(1910.0), px(280.0)));
    }

    #[test]
    fn drawer_is_clamped_to_small_work_area() {
        let bounds = drawer_bounds(Bounds::new(
            point(px(0.0), px(0.0)),
            size(px(200.0), px(100.0)),
        ));
        assert_eq!(bounds.origin, point(px(5.0), px(5.0)));
        assert_eq!(bounds.size, size(px(190.0), px(90.0)));
    }
}
