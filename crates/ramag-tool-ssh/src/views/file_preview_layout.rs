//! 远程文件弹窗的响应式尺寸计算。

const WIDTH_RATIO: f32 = 0.72;
const MIN_WIDTH: f32 = 640.0;
const MAX_WIDTH: f32 = 1_120.0;
const WINDOW_MARGIN: f32 = 32.0;
const MIN_VERTICAL_MARGIN: f32 = 24.0;
const DIALOG_CHROME_HEIGHT: f32 = 112.0;
const EDITOR_LINE_HEIGHT: f32 = 22.0;
const EDITOR_PADDING: f32 = 12.0;
const MIN_EDITOR_HEIGHT: f32 = 100.0;
const EXTRA_EDITOR_ROWS: usize = 3;

pub(super) struct RemoteFileDialogLayout {
    pub width: f32,
    pub editor_height: f32,
    pub margin_top: f32,
}

pub(super) fn remote_file_dialog_layout(
    viewport_width: f32,
    viewport_height: f32,
    content_lines: usize,
) -> RemoteFileDialogLayout {
    let available_width = (viewport_width - WINDOW_MARGIN).max(0.0);
    let width = (viewport_width * WIDTH_RATIO)
        .clamp(MIN_WIDTH, MAX_WIDTH)
        .min(available_width);
    let desired_rows = content_lines
        .saturating_add(EXTRA_EDITOR_ROWS)
        .max(EXTRA_EDITOR_ROWS);
    let desired_editor_height = desired_rows as f32 * EDITOR_LINE_HEIGHT + EDITOR_PADDING;
    let max_editor_height =
        (viewport_height - DIALOG_CHROME_HEIGHT - MIN_VERTICAL_MARGIN * 2.0).max(MIN_EDITOR_HEIGHT);
    let editor_height = desired_editor_height.min(max_editor_height);
    let dialog_height = editor_height + DIALOG_CHROME_HEIGHT;
    let margin_top = ((viewport_height - dialog_height) / 2.0).max(MIN_VERTICAL_MARGIN);

    RemoteFileDialogLayout {
        width,
        editor_height,
        margin_top,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_is_balanced_and_keeps_small_window_margin() {
        for (viewport, expected) in [
            (2_000.0, 1_120.0),
            (1_200.0, 864.0),
            (800.0, 640.0),
            (600.0, 568.0),
        ] {
            let actual = remote_file_dialog_layout(viewport, 1_000.0, 10).width;
            assert!(
                (actual - expected).abs() < 0.01,
                "viewport={viewport}, actual={actual}, expected={expected}"
            );
        }
    }

    #[test]
    fn editor_keeps_three_rows_after_short_content() {
        let layout = remote_file_dialog_layout(1_600.0, 1_300.0, 46);

        assert_eq!(layout.editor_height, 46.0 * 22.0 + 3.0 * 22.0 + 12.0);
        assert_eq!(
            remote_file_dialog_layout(1_600.0, 1_300.0, 0).editor_height,
            3.0 * 22.0 + 12.0
        );
    }

    #[test]
    fn short_dialog_is_centered_and_large_dialog_stays_inside_viewport() {
        let short = remote_file_dialog_layout(1_600.0, 1_200.0, 4);
        let large = remote_file_dialog_layout(1_600.0, 1_200.0, 50_000);

        assert!(short.margin_top > large.margin_top);
        assert_eq!(large.margin_top, MIN_VERTICAL_MARGIN);
        assert!(large.editor_height + DIALOG_CHROME_HEIGHT + large.margin_top * 2.0 <= 1_200.0);
    }
}
