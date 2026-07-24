//! 隐藏式双轴滚动：嵌套横向容器与纵向列表只消费一次手势的主方向。

use std::time::{Duration, Instant};

use gpui::{
    Context, Pixels, Point, ScrollHandle, ScrollWheelEvent, Styled, TouchPhase, Window, point, px,
};

/// Windows 普通滚轮事件没有 Started / Ended，以短暂停顿划分手势。
const GESTURE_IDLE_TIMEOUT: Duration = Duration::from_millis(150);
const SWITCH_MIN_DISTANCE: Pixels = px(5.0);
const SWITCH_DOMINANCE_RATIO: f32 = 1.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollAxis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct RoutedScroll {
    axis: ScrollAxis,
    delta: Pixels,
}

/// 跨渲染帧保留的一次滚动手势状态。
#[derive(Debug, Default)]
pub struct AxisScrollGesture {
    axis: Option<ScrollAxis>,
    last_event_at: Option<Instant>,
}

impl AxisScrollGesture {
    pub fn reset(&mut self) {
        self.axis = None;
        self.last_event_at = None;
    }

    fn route(
        &mut self,
        delta: Point<Pixels>,
        shift: bool,
        phase: TouchPhase,
        now: Instant,
    ) -> Option<RoutedScroll> {
        let gesture_expired = self.last_event_at.is_some_and(|last_event| {
            now.checked_duration_since(last_event)
                .is_some_and(|elapsed| elapsed > GESTURE_IDLE_TIMEOUT)
        });
        if matches!(phase, TouchPhase::Started) || gesture_expired {
            self.reset();
        }

        let ended = matches!(phase, TouchPhase::Ended);
        if delta.x == Pixels::ZERO && delta.y == Pixels::ZERO {
            if ended {
                self.reset();
            } else {
                self.last_event_at = Some(now);
            }
            return None;
        }

        let routed = if shift {
            self.axis = Some(ScrollAxis::Horizontal);
            RoutedScroll {
                axis: ScrollAxis::Horizontal,
                // 兼容仍把 Shift + 滚轮上报在 Y 轴的平台输入。
                delta: if delta.x == Pixels::ZERO {
                    delta.y
                } else {
                    delta.x
                },
            }
        } else {
            let horizontal = delta.x.abs();
            let vertical = delta.y.abs();
            let was_locked = self.axis.is_some();
            let mut axis = self.axis.unwrap_or_else(|| {
                let axis = if horizontal > vertical {
                    ScrollAxis::Horizontal
                } else {
                    ScrollAxis::Vertical
                };
                // 首个非零事件立即锁轴，避免低速触控板连续小位移分别推动两个方向。
                self.axis = Some(axis);
                axis
            });

            if was_locked {
                let should_switch = match axis {
                    ScrollAxis::Horizontal => {
                        vertical >= SWITCH_MIN_DISTANCE
                            && vertical > horizontal * SWITCH_DOMINANCE_RATIO
                    }
                    ScrollAxis::Vertical => {
                        horizontal >= SWITCH_MIN_DISTANCE
                            && horizontal > vertical * SWITCH_DOMINANCE_RATIO
                    }
                };
                if should_switch {
                    axis = match axis {
                        ScrollAxis::Horizontal => ScrollAxis::Vertical,
                        ScrollAxis::Vertical => ScrollAxis::Horizontal,
                    };
                    self.axis = Some(axis);
                }
            }

            RoutedScroll {
                axis,
                delta: match axis {
                    ScrollAxis::Horizontal => delta.x,
                    ScrollAxis::Vertical => delta.y,
                },
            }
        };

        if ended {
            self.reset();
        } else {
            self.last_event_at = Some(now);
        }
        Some(routed)
    }
}

/// 将滚轮事件分流到横向或纵向句柄。调用方应在两个原生滚动容器之上放置透明输入层。
pub fn handle_axis_scroll<T: 'static>(
    gesture: &mut AxisScrollGesture,
    event: &ScrollWheelEvent,
    window: &mut Window,
    horizontal: &ScrollHandle,
    vertical: &ScrollHandle,
    cx: &mut Context<T>,
) {
    let delta = event.delta.pixel_delta(window.line_height());
    let Some(routed) = gesture.route(
        delta,
        event.modifiers.shift,
        event.touch_phase,
        Instant::now(),
    ) else {
        return;
    };

    // 输入层手动分流后，阻止内层与外层原生容器再次消费同一事件。
    cx.stop_propagation();
    let handle = match routed.axis {
        ScrollAxis::Horizontal => horizontal,
        ScrollAxis::Vertical => vertical,
    };
    if apply_delta(handle, routed.axis, routed.delta) {
        cx.notify();
    }
}

fn apply_delta(handle: &ScrollHandle, axis: ScrollAxis, delta: Pixels) -> bool {
    let current = handle.offset();
    let max = handle.max_offset();
    let next = match axis {
        ScrollAxis::Horizontal => point((current.x + delta).clamp(-max.x, Pixels::ZERO), current.y),
        ScrollAxis::Vertical => point(current.x, (current.y + delta).clamp(-max.y, Pixels::ZERO)),
    };
    if next == current {
        return false;
    }
    handle.set_offset(next);
    true
}

/// 禁止单轴容器把另一轴滚轮位移自动映射过来；双轴分流由透明输入层负责。
pub trait RestrictScrollToAxisExt: Styled + Sized {
    fn restrict_scroll_to_axis(mut self) -> Self {
        self.style().restrict_scroll_to_axis = Some(true);
        self
    }
}

impl<T: Styled> RestrictScrollToAxisExt for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_lock_suppresses_vertical_noise() {
        let now = Instant::now();
        let mut gesture = AxisScrollGesture::default();

        let first = gesture.route(point(px(-40.0), px(-4.0)), false, TouchPhase::Moved, now);
        let noise = gesture.route(
            point(px(0.0), px(-4.0)),
            false,
            TouchPhase::Moved,
            now + Duration::from_millis(20),
        );

        assert_eq!(
            first.map(|scroll| scroll.axis),
            Some(ScrollAxis::Horizontal)
        );
        assert_eq!(noise.map(|scroll| scroll.delta), Some(Pixels::ZERO));
    }

    #[test]
    fn small_first_delta_still_locks_the_gesture_axis() {
        let now = Instant::now();
        let mut gesture = AxisScrollGesture::default();

        let first = gesture.route(point(px(-1.0), px(-0.2)), false, TouchPhase::Moved, now);
        let noise = gesture.route(
            point(px(0.0), px(-2.0)),
            false,
            TouchPhase::Moved,
            now + Duration::from_millis(20),
        );

        assert_eq!(
            first.map(|scroll| scroll.axis),
            Some(ScrollAxis::Horizontal)
        );
        assert_eq!(noise.map(|scroll| scroll.delta), Some(Pixels::ZERO));
    }

    #[test]
    fn dominant_cross_axis_motion_can_switch_lock() {
        let now = Instant::now();
        let mut gesture = AxisScrollGesture::default();
        let _ = gesture.route(point(px(-40.0), px(-4.0)), false, TouchPhase::Moved, now);

        let switched = gesture.route(
            point(px(-2.0), px(-20.0)),
            false,
            TouchPhase::Moved,
            now + Duration::from_millis(20),
        );

        assert_eq!(
            switched.map(|scroll| scroll.axis),
            Some(ScrollAxis::Vertical)
        );
    }

    #[test]
    fn idle_gap_starts_a_new_gesture() {
        let now = Instant::now();
        let mut gesture = AxisScrollGesture::default();
        let _ = gesture.route(point(px(-40.0), px(-4.0)), false, TouchPhase::Moved, now);

        let next = gesture.route(
            point(px(0.0), px(-20.0)),
            false,
            TouchPhase::Moved,
            now + GESTURE_IDLE_TIMEOUT + Duration::from_millis(1),
        );

        assert_eq!(next.map(|scroll| scroll.axis), Some(ScrollAxis::Vertical));
    }

    #[test]
    fn shift_wheel_routes_y_delta_horizontally() {
        let now = Instant::now();
        let mut gesture = AxisScrollGesture::default();

        let routed = gesture.route(point(px(0.0), px(-24.0)), true, TouchPhase::Moved, now);

        assert_eq!(
            routed,
            Some(RoutedScroll {
                axis: ScrollAxis::Horizontal,
                delta: px(-24.0),
            })
        );
    }
}
