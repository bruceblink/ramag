use super::*;
use gpui::TestAppContext;
use ramag_domain::entities::{ColumnKind, ColumnType};

fn column(name: &str, raw_type: &str, nullable: bool) -> Column {
    Column {
        name: name.into(),
        data_type: ColumnType {
            kind: ColumnKind::Other,
            raw_type: raw_type.into(),
        },
        nullable,
        default_value: None,
        is_primary_key: false,
        comment: None,
    }
}

fn designer(
    driver: DriverKind,
    columns: Vec<Column>,
    cx: &mut TestAppContext,
) -> (Entity<TableDesigner>, &mut gpui::VisualTestContext) {
    let mut designer = None;
    let (_, visual_cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            TableDesigner::new(
                TableDesignerConfig {
                    driver,
                    schema: "public".into(),
                    table: "users".into(),
                    columns,
                    loading: false,
                    ddl_loading: false,
                    on_execute: Rc::new(|_, _, _, _| true),
                    on_rename: Rc::new(|_, _, _, _, _, _| true),
                },
                window,
                cx,
            )
        });
        designer = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let Some(designer) = designer else {
        unreachable!("测试窗口应创建表设计器")
    };
    (designer, visual_cx)
}

#[gpui::test]
fn unchanged_columns_do_not_generate_sql(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (designer, cx) = designer(DriverKind::Mysql, vec![column("id", "int", false)], cx);

    let result = cx.update(|_, app| designer.read(app).change_sql(app));

    assert_eq!(result, Err(NO_CHANGES.into()));
    assert!(!cx.update(|_, app| designer.read(app).has_changes(app)));
}

#[gpui::test]
fn closing_changed_designer_requires_confirmation(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (designer, cx) = designer(DriverKind::Mysql, Vec::new(), cx);

    let unchanged_can_close =
        cx.update(|_, app| designer.update(app, |designer, cx| designer.allow_dialog_close(cx)));
    assert!(unchanged_can_close);

    cx.update(|window, app| {
        designer.update(app, |designer, cx| designer.add_field(window, cx));
    });
    let changed_can_close =
        cx.update(|_, app| designer.update(app, |designer, cx| designer.allow_dialog_close(cx)));

    assert!(!changed_can_close);
    assert!(cx.update(|_, app| designer.read(app).discard_confirming));
}

#[gpui::test]
fn added_fields_receive_unique_names(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (designer, cx) = designer(
        DriverKind::Mysql,
        vec![column("new_column", "varchar(255)", true)],
        cx,
    );
    cx.update(|window, app| {
        designer.update(app, |designer, cx| {
            designer.add_field(window, cx);
            designer.add_field(window, cx);
        });
    });

    let names = cx.update(|_, app| {
        designer
            .read(app)
            .fields
            .iter()
            .map(|field| field.name.read(app).value().to_string())
            .collect::<Vec<_>>()
    });

    assert_eq!(names, ["new_column", "new_column_2", "new_column_3"]);
}

#[gpui::test]
fn duplicate_field_names_are_rejected(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (designer, cx) = designer(
        DriverKind::Mysql,
        vec![
            column("first_name", "varchar(255)", true),
            column("last_name", "varchar(255)", true),
        ],
        cx,
    );
    cx.update(|window, app| {
        designer.update(app, |designer, cx| {
            designer.fields[1]
                .name
                .update(cx, |input, cx| input.set_value("first_name", window, cx));
        });
    });

    let error = cx
        .update(|_, app| designer.read(app).change_sql(app))
        .expect_err("重复字段名必须阻止 SQL 生成");

    assert!(error.contains("字段名 first_name 重复"));
}

#[gpui::test]
fn table_rename_uses_database_dialect(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for (driver, expected) in [
        (
            DriverKind::Mysql,
            "RENAME TABLE `public`.`users` TO `public`.`members`;",
        ),
        (
            DriverKind::Postgres,
            "ALTER TABLE \"public\".\"users\" RENAME TO \"members\";",
        ),
    ] {
        let (designer, visual_cx) = designer(driver, Vec::new(), cx);
        visual_cx.update(|window, app| {
            designer.update(app, |designer, cx| {
                designer
                    .table_name
                    .update(cx, |input, cx| input.set_value("members", window, cx));
            });
        });

        let sql = visual_cx
            .update(|_, app| designer.read(app).rename_sql(app))
            .expect("改表名应生成 SQL");
        assert_eq!(sql, expected);
    }
}

#[gpui::test]
fn table_name_change_is_not_included_in_field_sql(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (designer, cx) = designer(DriverKind::Mysql, Vec::new(), cx);
    cx.update(|window, app| {
        designer.update(app, |designer, cx| {
            designer
                .table_name
                .update(cx, |input, cx| input.set_value("members", window, cx));
        });
    });

    let (field_sql, table_changed, fields_changed) = cx.update(|_, app| {
        let designer = designer.read(app);
        (
            designer.change_sql(app),
            designer.has_table_name_change(app),
            designer.has_field_changes(app),
        )
    });

    assert_eq!(field_sql, Err(NO_CHANGES.into()));
    assert!(table_changed);
    assert!(!fields_changed);
}

#[gpui::test]
fn mysql_add_column_uses_mysql_dialect(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (designer, cx) = designer(DriverKind::Mysql, Vec::new(), cx);
    cx.update(|window, app| {
        designer.update(app, |designer, cx| designer.add_field(window, cx));
    });

    let sql = cx
        .update(|_, app| designer.read(app).change_sql(app))
        .expect("新增字段应生成 SQL");

    assert_eq!(
        sql,
        "ALTER TABLE `public`.`users` ADD COLUMN `new_column` VARCHAR(255) NULL;"
    );
}

#[gpui::test]
fn mysql_batches_multiple_column_changes_into_one_alter(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (designer, cx) = designer(DriverKind::Mysql, Vec::new(), cx);
    cx.update(|window, app| {
        designer.update(app, |designer, cx| {
            designer.add_field(window, cx);
            designer.add_field(window, cx);
        });
    });

    let sql = cx
        .update(|_, app| designer.read(app).change_sql(app))
        .expect("多个字段应生成 SQL");

    assert_eq!(sql.matches("ALTER TABLE").count(), 1);
    assert_eq!(sql.matches("ADD COLUMN").count(), 2);
    assert!(sql.contains(",\n    ADD COLUMN"));
}

#[gpui::test]
fn postgres_changes_emit_separate_alter_statements(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (designer, cx) = designer(DriverKind::Postgres, vec![column("name", "text", true)], cx);
    cx.update(|window, app| {
        designer.update(app, |designer, cx| {
            let field = &mut designer.fields[0];
            field.nullable = false;
            field
                .comment
                .update(cx, |input, cx| input.set_value("显示名称", window, cx));
        });
    });

    let sql = cx
        .update(|_, app| designer.read(app).change_sql(app))
        .expect("修改字段应生成 SQL");

    assert_eq!(
        sql,
        "ALTER TABLE \"public\".\"users\" ALTER COLUMN \"name\" SET NOT NULL;\n\
             COMMENT ON COLUMN \"public\".\"users\".\"name\" IS '显示名称';"
    );
}

#[test]
fn default_value_semantics_choose_expected_colors() {
    let keyword = gpui::red();
    let number = gpui::green();
    let string = gpui::blue();
    let constant = gpui::white();

    assert_eq!(
        default_value_color("CURRENT_TIMESTAMP", keyword, number, string, constant),
        keyword
    );
    assert_eq!(
        default_value_color("42.5", keyword, number, string, constant),
        number
    );
    assert_eq!(
        default_value_color("'draft'", keyword, number, string, constant),
        string
    );
}

#[test]
fn many_fields_use_fixed_visible_row_count() {
    assert_eq!(visible_field_rows(14, false), MAX_VISIBLE_FIELD_ROWS);
    assert_eq!(visible_field_rows(14, true), 5);
    assert_eq!(visible_field_rows(0, false), 1);
}
