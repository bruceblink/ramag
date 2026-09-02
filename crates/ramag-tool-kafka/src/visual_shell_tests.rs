use super::*;

/// 验证窄窗口下侧栏、主标题栏和工作区操作区都遵守 680px 的外壳边界。
pub(super) fn assert_compact_shell(
    visual_cx: &mut VisualTestContext,
    _kafka_entity: &gpui::Entity<KafkaView>,
) {
    visual_cx.run_until_parked();
    let bounds = [
        visual_cx.debug_bounds("kafka-root"),
        visual_cx.debug_bounds("kafka-sidebar"),
        visual_cx.debug_bounds("kafka-main"),
        visual_cx.debug_bounds("kafka-header"),
        visual_cx.debug_bounds("kafka-header-actions"),
        visual_cx.debug_bounds("kafka-workspace-tabs"),
        visual_cx.debug_bounds("kafka-config-actions"),
    ];
    assert!(
        bounds.iter().all(Option::is_some),
        "compact Kafka controls should all be rendered"
    );
    let [
        Some(compact_root),
        Some(compact_sidebar),
        Some(compact_main),
        Some(compact_header),
        Some(compact_header_actions),
        Some(compact_tabs),
        Some(compact_config_actions),
    ] = bounds
    else {
        return;
    };

    assert!(compact_root.size.width <= px(680.0));
    assert!(compact_sidebar.size.width <= px(680.0));
    assert!(compact_main.size.width <= px(680.0));
    assert!(compact_sidebar.origin.y + compact_sidebar.size.height <= compact_main.origin.y);
    assert!(compact_header_actions.origin.x >= compact_header.origin.x);
    assert!(
        compact_header_actions.origin.x + compact_header_actions.size.width
            <= compact_header.origin.x + compact_header.size.width
    );
    assert!(compact_tabs.size.width <= compact_main.size.width);
    assert!(compact_config_actions.origin.x >= compact_tabs.origin.x);
    assert!(
        compact_config_actions.origin.x + compact_config_actions.size.width
            <= compact_tabs.origin.x + compact_tabs.size.width
    );
}
