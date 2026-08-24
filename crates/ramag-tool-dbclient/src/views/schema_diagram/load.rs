use std::sync::Arc;

use ramag_app::ConnectionService;
use ramag_domain::entities::ConnectionConfig;

use super::{
    DiagramColumn, DiagramRelation, DiagramTable, LoadedDiagram, MAX_DIAGRAM_COLUMNS,
    MAX_DIAGRAM_RELATIONS_PER_TABLE, MAX_DIAGRAM_TABLES,
};

/// Loads bounded table metadata concurrently and returns nodes plus metadata-backed edges.
pub(super) async fn load_diagram(
    service: Arc<ConnectionService>,
    connection: ConnectionConfig,
    schema: String,
) -> Result<LoadedDiagram, String> {
    let tables = service
        .list_tables(&connection, &schema)
        .await
        .map_err(|error| error.to_string())?;
    let omitted_tables = tables.len().saturating_sub(MAX_DIAGRAM_TABLES);
    let tables = tables.into_iter().take(MAX_DIAGRAM_TABLES);
    let futures = tables.map(|table| {
        let service = service.clone();
        let connection = connection.clone();
        let schema = schema.clone();
        async move {
            let table_name = table.name.clone();
            let (columns_result, foreign_keys_result) = futures::join!(
                service.list_columns(&connection, &schema, &table_name),
                service.list_foreign_keys(&connection, &schema, &table_name)
            );
            let mut metadata_errors = Vec::new();
            let columns = match columns_result {
                Ok(columns) => columns,
                Err(error) => {
                    metadata_errors.push(format!("列：{error}"));
                    Vec::new()
                }
            };
            let foreign_keys = match foreign_keys_result {
                Ok(keys) => keys,
                Err(error) => {
                    metadata_errors.push(format!("外键：{error}"));
                    Vec::new()
                }
            };
            let relations: Vec<DiagramRelation> = foreign_keys
                .into_iter()
                .take(MAX_DIAGRAM_RELATIONS_PER_TABLE)
                .map(|foreign_key| DiagramRelation {
                    source_table: table.name.clone(),
                    name: foreign_key.name,
                    columns: foreign_key.columns,
                    ref_schema: foreign_key.ref_schema,
                    ref_table: foreign_key.ref_table,
                    ref_columns: foreign_key.ref_columns,
                })
                .collect();
            let columns = columns
                .into_iter()
                .take(MAX_DIAGRAM_COLUMNS)
                .map(|column| DiagramColumn {
                    name: column.name,
                    raw_type: column.data_type.raw_type,
                    nullable: column.nullable,
                    primary: column.is_primary_key,
                })
                .collect();
            DiagramTable {
                name: table.name,
                is_view: table.is_view,
                comment: table.comment,
                columns,
                relations,
                metadata_error: (!metadata_errors.is_empty()).then(|| metadata_errors.join("；")),
            }
        }
    });
    let tables: Vec<DiagramTable> = futures::future::join_all(futures).await;
    let relations = tables
        .iter()
        .flat_map(|table| table.relations.iter().cloned())
        .collect();
    Ok(LoadedDiagram {
        tables,
        relations,
        omitted_tables,
    })
}

#[cfg(test)]
mod tests {
    use super::super::{CARD_GAP, CARD_WIDTH, GRID_COLUMNS};

    #[test]
    fn diagram_width_keeps_a_wide_schema_scrollable() {
        let visible_tables = 9usize;
        let columns = visible_tables.clamp(1, GRID_COLUMNS);
        let width =
            (columns as f32 * CARD_WIDTH + columns.saturating_sub(1) as f32 * CARD_GAP + 24.0)
                .max(640.0);
        assert!(width > 640.0);
        assert_eq!(columns, GRID_COLUMNS);
    }

    #[test]
    fn empty_diagram_still_has_a_stable_minimum_canvas() {
        let columns = 0usize.clamp(1, GRID_COLUMNS);
        let width =
            (columns as f32 * CARD_WIDTH + columns.saturating_sub(1) as f32 * CARD_GAP + 24.0)
                .max(640.0);
        assert_eq!(columns, 1);
        assert_eq!(width, 640.0);
    }
}
