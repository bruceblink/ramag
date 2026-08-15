use super::*;

impl TableDesigner {
    pub(super) fn change_sql(&self, cx: &gpui::App) -> Result<String, String> {
        let qualified = format!(
            "{}.{}",
            self.driver.quote_identifier(&self.schema),
            self.driver.quote_identifier(&self.original_table)
        );
        let mut statements = Vec::new();
        let mut mysql_alter_clauses = Vec::new();
        let mut names = HashSet::new();
        for field in &self.fields {
            let name = field.name.read(cx).value().trim().to_string();
            let data_type = field.data_type.read(cx).value().trim().to_string();
            let default_value = field.default_value.read(cx).value().trim().to_string();
            let comment = field.comment.read(cx).value().trim().to_string();
            if field.deleted {
                if let Some(original) = &field.original {
                    let column = self.driver.quote_identifier(&original.name);
                    if self.driver == DriverKind::Mysql {
                        mysql_alter_clauses.push(format!("DROP COLUMN {column}"));
                    } else {
                        statements.push(format!("ALTER TABLE {qualified} DROP COLUMN {column};"));
                    }
                }
                continue;
            }
            validate_identifier("字段名", &name)?;
            if !names.insert(name.to_ascii_lowercase()) {
                return Err(format!("字段名 {name} 重复，请修改后再预览"));
            }
            if data_type.is_empty() {
                return Err(format!("字段 {name} 的类型不能为空"));
            }
            if data_type.contains(';') {
                return Err(format!("字段 {name} 的类型不能包含分号"));
            }
            if default_value.contains(';') {
                return Err(format!("字段 {name} 的默认值不能包含分号"));
            }
            let sql = FieldSql {
                name: &name,
                data_type: &data_type,
                default_value: &default_value,
                comment: &comment,
            };
            match self.driver {
                DriverKind::Mysql => self.mysql_field_sql(field, &sql, &mut mysql_alter_clauses),
                DriverKind::Postgres => {
                    self.postgres_field_sql(field, &qualified, &sql, &mut statements)
                }
                _ => return Err("当前数据库不支持表结构设计器".into()),
            }
        }
        if !mysql_alter_clauses.is_empty() {
            statements.push(format!(
                "ALTER TABLE {qualified} {};",
                mysql_alter_clauses.join(",\n    ")
            ));
        }
        if statements.is_empty() {
            Err(NO_CHANGES.into())
        } else {
            Ok(statements.join("\n"))
        }
    }

    pub(super) fn rename_sql(&self, cx: &gpui::App) -> Result<String, String> {
        let table = self.table_name.read(cx).value().trim().to_string();
        validate_identifier("表名", &table)?;
        if self.original_table == table {
            return Err(NO_CHANGES.into());
        }
        let schema = self.driver.quote_identifier(&self.schema);
        let old = self.driver.quote_identifier(&self.original_table);
        let new = self.driver.quote_identifier(&table);
        match self.driver {
            DriverKind::Mysql => Ok(format!("RENAME TABLE {schema}.{old} TO {schema}.{new};")),
            DriverKind::Postgres => Ok(format!("ALTER TABLE {schema}.{old} RENAME TO {new};")),
            _ => Err("当前数据库不支持表结构设计器".into()),
        }
    }
    fn mysql_field_sql(&self, field: &FieldDraft, sql: &FieldSql<'_>, out: &mut Vec<String>) {
        let definition = mysql_definition(
            self.driver,
            sql.name,
            sql.data_type,
            field.nullable,
            sql.default_value,
            sql.comment,
        );
        match &field.original {
            None => out.push(format!("ADD COLUMN {definition}")),
            Some(original)
                if field_changed(
                    field,
                    original,
                    sql.name,
                    sql.data_type,
                    sql.default_value,
                    sql.comment,
                ) =>
            {
                out.push(format!(
                    "CHANGE COLUMN {} {definition}",
                    self.driver.quote_identifier(&original.name)
                ))
            }
            _ => {}
        }
    }

    fn postgres_field_sql(
        &self,
        field: &FieldDraft,
        table: &str,
        sql: &FieldSql<'_>,
        out: &mut Vec<String>,
    ) {
        let qname = self.driver.quote_identifier(sql.name);
        let Some(original) = &field.original else {
            let null = if field.nullable { "" } else { " NOT NULL" };
            let default = if sql.default_value.is_empty() {
                String::new()
            } else {
                format!(" DEFAULT {}", sql.default_value)
            };
            out.push(format!(
                "ALTER TABLE {table} ADD COLUMN {qname} {}{null}{default};",
                sql.data_type
            ));
            if !sql.comment.is_empty() {
                out.push(format!(
                    "COMMENT ON COLUMN {table}.{qname} IS '{}';",
                    escape_literal(sql.comment)
                ));
            }
            return;
        };
        if original.name != sql.name {
            out.push(format!(
                "ALTER TABLE {table} RENAME COLUMN {} TO {qname};",
                self.driver.quote_identifier(&original.name)
            ));
        }
        if original.data_type.raw_type != sql.data_type {
            out.push(format!(
                "ALTER TABLE {table} ALTER COLUMN {qname} TYPE {};",
                sql.data_type
            ));
        }
        if original.nullable != field.nullable {
            out.push(format!(
                "ALTER TABLE {table} ALTER COLUMN {qname} {};",
                if field.nullable {
                    "DROP NOT NULL"
                } else {
                    "SET NOT NULL"
                }
            ));
        }
        if original.default_value.as_deref().unwrap_or("") != sql.default_value {
            out.push(if sql.default_value.is_empty() {
                format!("ALTER TABLE {table} ALTER COLUMN {qname} DROP DEFAULT;")
            } else {
                format!(
                    "ALTER TABLE {table} ALTER COLUMN {qname} SET DEFAULT {};",
                    sql.default_value
                )
            });
        }
        if original.comment.as_deref().unwrap_or("") != sql.comment {
            out.push(format!(
                "COMMENT ON COLUMN {table}.{qname} IS {};",
                if sql.comment.is_empty() {
                    "NULL".into()
                } else {
                    format!("'{}'", escape_literal(sql.comment))
                }
            ));
        }
    }
}

fn validate_identifier(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{label}不能为空"));
    }
    if value.len() > MAX_CONNECTION_IDENTIFIER_BYTES {
        return Err(format!(
            "{label}不能超过 {MAX_CONNECTION_IDENTIFIER_BYTES} 字节"
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{label}不能包含控制字符"));
    }
    Ok(())
}
fn mysql_definition(
    driver: DriverKind,
    name: &str,
    data_type: &str,
    nullable: bool,
    default_value: &str,
    comment: &str,
) -> String {
    let null = if nullable { " NULL" } else { " NOT NULL" };
    let default = if default_value.is_empty() {
        String::new()
    } else {
        format!(" DEFAULT {default_value}")
    };
    let comment = if comment.is_empty() {
        String::new()
    } else {
        format!(" COMMENT '{}'", escape_literal(comment))
    };
    format!(
        "{} {data_type}{null}{default}{comment}",
        driver.quote_identifier(name)
    )
}

fn escape_literal(value: &str) -> String {
    value.replace('\'', "''")
}
