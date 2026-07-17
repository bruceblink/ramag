use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use parking_lot::RwLock;

const MAX_CACHED_TABLE_SCHEMAS: usize = 64;
const MAX_CACHED_TABLE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CACHED_COLUMN_TABLES: usize = 512;
const MAX_CACHED_COLUMN_BYTES: usize = 16 * 1024 * 1024;
const MAX_IN_FLIGHT_TABLE_REFRESHES: usize = 256;
const MAX_IN_FLIGHT_COLUMN_LOADS: usize = 128;

/// Schema 元数据缓存：表 / 视图 / 列 / schema 列表，供补全与视图判定共用。
#[derive(Default)]
pub struct SchemaCache {
    pub tables: HashMap<String, Vec<String>>,
    pub views: HashMap<String, HashSet<String>>,
    pub columns: HashMap<(String, String), Vec<String>>,
    loading_columns: HashSet<(String, String)>,
    pub default_schema: Option<String>,
    pub all_schemas: Vec<String>,
    pub show_system: bool,
    table_order: VecDeque<String>,
    column_order: VecDeque<(String, String)>,
    table_bytes: usize,
    column_bytes: usize,
    table_refresh_sequence: u64,
    table_refreshes: HashMap<String, u64>,
}

impl SchemaCache {
    pub fn new_shared() -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self::default()))
    }

    pub fn is_view(&self, schema: Option<&str>, table: &str) -> bool {
        let Some(schema) = schema else {
            return false;
        };
        self.views
            .get(schema)
            .is_some_and(|views| views.iter().any(|view| view.eq_ignore_ascii_case(table)))
    }

    pub fn all_tables(&self) -> Vec<String> {
        let mut output = Vec::new();
        if let Some(default) = &self.default_schema
            && let Some(tables) = self.tables.get(default)
        {
            output.extend(tables.iter().cloned());
        }
        for (schema, tables) in &self.tables {
            if Some(schema) != self.default_schema.as_ref() {
                output.extend(tables.iter().cloned());
            }
        }
        output
    }

    /// 为一次 schema 表列表请求分配代次；较慢旧回包不得覆盖较新结果。
    pub fn begin_table_refresh(&mut self, schema: &str) -> u64 {
        self.table_refresh_sequence = self.table_refresh_sequence.wrapping_add(1);
        if self.table_refresh_sequence == 0 {
            self.table_refresh_sequence = 1;
        }
        let generation = self.table_refresh_sequence;
        if !self.table_refreshes.contains_key(schema)
            && self.table_refreshes.len() >= MAX_IN_FLIGHT_TABLE_REFRESHES
            && let Some(oldest) = self
                .table_refreshes
                .iter()
                .min_by_key(|(_, generation)| **generation)
                .map(|(schema, _)| schema.clone())
        {
            self.table_refreshes.remove(&oldest);
        }
        self.table_refreshes.insert(schema.to_string(), generation);
        generation
    }

    pub fn finish_table_refresh(
        &mut self,
        schema: String,
        generation: u64,
        tables: Vec<String>,
        views: HashSet<String>,
    ) -> bool {
        if self.table_refreshes.get(&schema).copied() != Some(generation) {
            return false;
        }
        self.table_refreshes.remove(&schema);
        self.cache_tables(schema, tables, views)
    }

    pub fn cancel_table_refresh(&mut self, schema: &str, generation: u64) {
        if self.table_refreshes.get(schema).copied() == Some(generation) {
            self.table_refreshes.remove(schema);
        }
    }

    pub fn cache_tables(
        &mut self,
        schema: String,
        tables: Vec<String>,
        views: HashSet<String>,
    ) -> bool {
        self.remove_table_entry(&schema);
        let bytes = table_entry_bytes(&schema, &tables, &views);
        if bytes > MAX_CACHED_TABLE_BYTES {
            return false;
        }
        while self.tables.len() >= MAX_CACHED_TABLE_SCHEMAS
            || self.table_bytes.saturating_add(bytes) > MAX_CACHED_TABLE_BYTES
        {
            let Some(oldest) = self.table_order.pop_front() else {
                break;
            };
            self.remove_table_entry(&oldest);
        }
        self.table_bytes = self.table_bytes.saturating_add(bytes);
        self.table_order.push_back(schema.clone());
        self.tables.insert(schema.clone(), tables);
        self.views.insert(schema, views);
        true
    }

    pub fn cache_columns(&mut self, key: (String, String), columns: Vec<String>) -> bool {
        self.remove_column_entry(&key);
        let bytes = column_entry_bytes(&key, &columns);
        if bytes > MAX_CACHED_COLUMN_BYTES {
            return false;
        }
        while self.columns.len() >= MAX_CACHED_COLUMN_TABLES
            || self.column_bytes.saturating_add(bytes) > MAX_CACHED_COLUMN_BYTES
        {
            let Some(oldest) = self.column_order.pop_front() else {
                break;
            };
            self.remove_column_entry(&oldest);
        }
        self.column_bytes = self.column_bytes.saturating_add(bytes);
        self.column_order.push_back(key.clone());
        self.columns.insert(key, columns);
        true
    }

    /// 抢占列元数据加载名额；相同表防重，且限制慢连接下的在途请求数量。
    pub fn begin_column_load(&mut self, key: (String, String)) -> bool {
        if self.columns.contains_key(&key)
            || self.loading_columns.contains(&key)
            || self.loading_columns.len() >= MAX_IN_FLIGHT_COLUMN_LOADS
        {
            return false;
        }
        self.loading_columns.insert(key)
    }

    pub fn finish_column_load(&mut self, key: &(String, String)) {
        self.loading_columns.remove(key);
    }

    pub fn invalidate_table(&mut self, schema: &str, table: &str) {
        self.remove_table_entry(schema);
        self.remove_column_entry(&(schema.to_string(), table.to_string()));
        self.loading_columns
            .remove(&(schema.to_string(), table.to_string()));
    }

    pub fn invalidate_schema(&mut self, schema: &str) {
        self.remove_table_entry(schema);
        let keys: Vec<(String, String)> = self
            .columns
            .keys()
            .filter(|(cached_schema, _)| cached_schema == schema)
            .cloned()
            .collect();
        for key in keys {
            self.remove_column_entry(&key);
        }
        self.loading_columns
            .retain(|(cached_schema, _)| cached_schema != schema);
    }

    fn remove_table_entry(&mut self, schema: &str) {
        let tables = self.tables.remove(schema);
        let views = self.views.remove(schema);
        if let Some(tables) = tables {
            let empty_views = HashSet::new();
            self.table_bytes = self.table_bytes.saturating_sub(table_entry_bytes(
                schema,
                &tables,
                views.as_ref().unwrap_or(&empty_views),
            ));
        }
        self.table_order.retain(|cached| cached != schema);
    }

    fn remove_column_entry(&mut self, key: &(String, String)) {
        if let Some(columns) = self.columns.remove(key) {
            self.column_bytes = self
                .column_bytes
                .saturating_sub(column_entry_bytes(key, &columns));
        }
        self.column_order.retain(|cached| cached != key);
    }
}

fn string_bytes(value: &str) -> usize {
    std::mem::size_of::<String>().saturating_add(value.len())
}

fn table_entry_bytes(schema: &str, tables: &[String], views: &HashSet<String>) -> usize {
    let table_bytes = tables.iter().fold(0usize, |total, table| {
        total.saturating_add(string_bytes(table))
    });
    let view_bytes = views.iter().fold(0usize, |total, view| {
        total
            .saturating_add(string_bytes(view))
            .saturating_add(3 * std::mem::size_of::<usize>())
    });
    string_bytes(schema)
        .saturating_add(table_bytes)
        .saturating_add(view_bytes)
}

fn column_entry_bytes(key: &(String, String), columns: &[String]) -> usize {
    columns.iter().fold(
        string_bytes(&key.0).saturating_add(string_bytes(&key.1)),
        |total, column| total.saturating_add(string_bytes(column)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_table_refresh_cannot_overwrite_newer_result() {
        let mut cache = SchemaCache::default();
        let old = cache.begin_table_refresh("public");
        let new = cache.begin_table_refresh("public");

        assert!(cache.finish_table_refresh(
            "public".into(),
            new,
            vec!["new_table".into()],
            HashSet::new(),
        ));
        assert!(!cache.finish_table_refresh(
            "public".into(),
            old,
            vec!["old_table".into()],
            HashSet::new(),
        ));
        assert_eq!(cache.tables["public"], ["new_table"]);
    }

    #[test]
    fn table_refresh_generation_never_uses_zero_sentinel() {
        let mut cache = SchemaCache {
            table_refresh_sequence: u64::MAX,
            ..SchemaCache::default()
        };

        assert_eq!(cache.begin_table_refresh("public"), 1);
    }

    #[test]
    fn table_and_column_caches_evict_old_entries() {
        let mut cache = SchemaCache::default();
        for index in 0..=MAX_CACHED_TABLE_SCHEMAS {
            assert!(cache.cache_tables(
                format!("schema_{index}"),
                vec!["table".into()],
                HashSet::new(),
            ));
        }
        assert_eq!(cache.tables.len(), MAX_CACHED_TABLE_SCHEMAS);
        assert!(!cache.tables.contains_key("schema_0"));

        for index in 0..=MAX_CACHED_COLUMN_TABLES {
            assert!(cache.cache_columns(
                ("public".into(), format!("table_{index}")),
                vec!["id".into()],
            ));
        }
        assert_eq!(cache.columns.len(), MAX_CACHED_COLUMN_TABLES);
        assert!(
            !cache
                .columns
                .contains_key(&("public".into(), "table_0".into()))
        );
    }

    #[test]
    fn in_flight_column_loads_are_bounded_and_reusable() {
        let mut cache = SchemaCache::default();
        for index in 0..MAX_IN_FLIGHT_COLUMN_LOADS {
            assert!(cache.begin_column_load(("public".into(), format!("table_{index}"))));
        }
        assert!(!cache.begin_column_load(("public".into(), "overflow".into())));

        let completed = ("public".to_string(), "table_0".to_string());
        cache.finish_column_load(&completed);
        assert!(cache.begin_column_load(("public".into(), "replacement".into())));
    }

    #[test]
    fn oversized_single_cache_entry_is_not_retained() {
        let mut cache = SchemaCache::default();
        assert!(!cache.cache_columns(
            ("public".into(), "huge".into()),
            vec!["x".repeat(MAX_CACHED_COLUMN_BYTES)],
        ));
        assert!(cache.columns.is_empty());
    }
}
