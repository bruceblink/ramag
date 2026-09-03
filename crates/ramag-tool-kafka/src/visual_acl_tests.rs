use super::*;

pub(super) fn exercise_acl_workspace(
    visual_cx: &mut VisualTestContext,
    kafka_entity: &gpui::Entity<KafkaView>,
) {
    click(visual_cx, "kafka-section-Acls");
    visual_cx.run_until_parked();
    assert!(visual_cx.debug_bounds("kafka-acls").is_some());
    assert!(visual_cx.debug_bounds("kafka-acl-filter").is_some());
    assert!(visual_cx.debug_bounds("kafka-acl-filter-header").is_some());
    assert!(visual_cx.debug_bounds("kafka-acl-admin").is_some());
    assert!(visual_cx.debug_bounds("kafka-acl-admin-header").is_some());
    assert!(
        visual_cx
            .debug_bounds("kafka-acl-row-User:ramag-Topic-ramag.integration.messages-READ-ALLOW")
            .is_some(),
        "ACL 列表应显示真实驱动返回的规则"
    );
    assert!(visual_cx.debug_bounds("kafka-acl-detail").is_some());
    assert!(visual_cx.debug_bounds("kafka-acl-delete").is_some());
    assert_within_width(visual_cx, "kafka-acl-filter", 1200.0);
    assert_within_width(visual_cx, "kafka-acl-admin", 1200.0);
    assert_within_width(visual_cx, "kafka-acl-detail", 1200.0);

    visual_cx.update(|window, app| {
        kafka_entity.update(app, |view, cx| {
            view.read_only = KafkaReadOnlyState::ReadWrite;
            view.acl_principal
                .update(cx, |input, cx| input.set_value("User:ramag", window, cx));
            view.acl_host
                .update(cx, |input, cx| input.set_value("*", window, cx));
            view.acl_resource_name.update(cx, |input, cx| {
                input.set_value("ramag.integration.messages", window, cx)
            });
            cx.notify();
        });
    });
    visual_cx.run_until_parked();
    click(visual_cx, "kafka-acl-create");
    visual_cx.run_until_parked();
    assert!(
        visual_cx.debug_bounds("ramag-confirm-ok").is_some(),
        "创建 ACL 前必须显示确认对话框"
    );
    click(visual_cx, "ramag-confirm-cancel");
    visual_cx.run_until_parked();
    assert!(visual_cx.debug_bounds("ramag-confirm-ok").is_none());
    click(visual_cx, "kafka-acl-delete");
    visual_cx.run_until_parked();
    assert!(
        visual_cx.debug_bounds("ramag-confirm-ok").is_some(),
        "删除 ACL 前必须显示确认对话框"
    );
    click(visual_cx, "ramag-confirm-cancel");
    visual_cx.run_until_parked();
    assert!(visual_cx.debug_bounds("ramag-confirm-ok").is_none());

    for (width, height) in [(360.0, 900.0), (1024.0, 900.0), (1440.0, 900.0)] {
        visual_cx.simulate_resize(size(px(width), px(height)));
        visual_cx.run_until_parked();
        for selector in [
            "kafka-acls",
            "kafka-acl-filter",
            "kafka-acl-filter-header",
            "kafka-acl-admin",
            "kafka-acl-admin-header",
            "kafka-acl-list-panel",
            "kafka-acl-detail",
        ] {
            assert_within_width(visual_cx, selector, width);
        }
        let Some(list) = visual_cx.debug_bounds("kafka-acl-list-panel") else {
            return;
        };
        let Some(detail) = visual_cx.debug_bounds("kafka-acl-detail") else {
            return;
        };
        if width < 1060.0 {
            assert!(
                list.origin.y + list.size.height <= detail.origin.y,
                "紧凑窗口中 ACL 列表和详情不应重叠: list={list:?}, detail={detail:?}"
            );
        } else {
            assert!(
                list.origin.x + list.size.width <= detail.origin.x,
                "宽窗口中 ACL 列表和详情不应重叠: list={list:?}, detail={detail:?}"
            );
        }
    }
    visual_cx.simulate_resize(size(px(1200.0), px(780.0)));
    visual_cx.run_until_parked();
}
