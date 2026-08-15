pub(super) struct ExplorerLayout {
    pub(super) compact: bool,
    pub(super) show_mounts: bool,
    pub(super) show_detail: bool,
    pub(super) detail_fullscreen: bool,
}

impl ExplorerLayout {
    pub(super) fn resolve(
        viewport_width: f32,
        requested_mounts: bool,
        requested_detail: bool,
    ) -> Self {
        let compact = viewport_width < 820.0;
        Self {
            compact,
            show_mounts: !compact || requested_mounts,
            show_detail: requested_detail,
            detail_fullscreen: compact,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ExplorerLayout;

    #[test]
    fn wide_workspace_keeps_navigation_and_uses_optional_detail_drawer() {
        let normal = ExplorerLayout::resolve(1200.0, false, false);
        assert!(normal.show_mounts);
        assert!(!normal.show_detail);
        assert!(!normal.detail_fullscreen);

        let with_detail = ExplorerLayout::resolve(1200.0, true, true);
        assert!(with_detail.show_mounts);
        assert!(with_detail.show_detail);
        assert!(!with_detail.detail_fullscreen);
    }

    #[test]
    fn compact_workspace_respects_toggles_and_expands_detail() {
        let hidden = ExplorerLayout::resolve(700.0, false, false);
        assert!(!hidden.show_mounts);
        assert!(!hidden.show_detail);

        let detail = ExplorerLayout::resolve(700.0, false, true);
        assert!(!detail.show_mounts);
        assert!(detail.show_detail);
        assert!(detail.detail_fullscreen);
    }
}
