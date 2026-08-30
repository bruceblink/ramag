use std::sync::Arc;

use super::{
    KafkaSection, KafkaTool, KafkaView, bytes_to_base64, bytes_to_hex, format_message_json,
    parse_bootstrap_servers, parse_datetime_text, parse_partition_list,
};
use async_trait::async_trait;
use gpui::{
    AppContext as _, Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px, size,
};
use ramag_app::KafkaService;
use ramag_domain::entities::KafkaMessageRecord;
use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, KafkaBroker, KafkaClusterConfig, KafkaClusterMetadata,
    KafkaMessagePage, KafkaMessageQuery, KafkaMessageSearchQuery, KafkaPartition, KafkaTopic,
    QueryRecord, QueryRecordId,
};
use ramag_domain::error::Result;
use ramag_domain::traits::{KafkaDriver, Storage, Tool};

#[test]
fn tool_metadata_exposes_kafka_entry() {
    let tool = KafkaTool::new();
    assert_eq!(tool.meta().id, "kafka");
    assert_eq!(tool.meta().name, "Kafka");
    assert_eq!(tool.meta().icon.as_deref(), Some("server"));
}

#[test]
fn bootstrap_input_accepts_common_separators_without_fake_values() {
    assert_eq!(
        parse_bootstrap_servers(" broker-a:9092,\nbroker-b:9092\r\n"),
        vec!["broker-a:9092", "broker-b:9092"]
    );
    assert!(parse_bootstrap_servers(" , \n ").is_empty());
}

#[test]
fn sections_keep_the_read_only_workflow_order() {
    assert_eq!(KafkaSection::ALL[0], KafkaSection::Overview);
    assert_eq!(KafkaSection::ALL[1], KafkaSection::Topics);
    assert_eq!(KafkaSection::ALL[2], KafkaSection::Messages);
    assert_eq!(KafkaSection::ALL[3], KafkaSection::Config);
}

#[test]
fn partition_input_accepts_multiple_values_and_rejects_duplicates() {
    assert_eq!(parse_partition_list("0, 2\n4"), Ok(vec![0, 2, 4]));
    assert!(parse_partition_list("0,0").is_err());
    assert!(parse_partition_list("-1").is_err());
    assert!(parse_partition_list(" ").is_err());
}

#[test]
fn message_formats_preserve_binary_values() {
    assert_eq!(bytes_to_hex(&[0, 15, 255]), "000fff");
    assert_eq!(bytes_to_base64(b"hello"), "aGVsbG8=");
    let record = KafkaMessageRecord {
        topic: "events".into(),
        partition: 1,
        offset: 2,
        timestamp: None,
        key: Some(vec![0xff]),
        value: Some(vec![0, 1, 2]),
        headers: vec![ramag_domain::entities::KafkaMessageHeader {
            key: "trace".into(),
            value: Some(vec![0xfe]),
        }],
    };
    let json = format_message_json(&record);
    assert!(json.contains("value_base64"));
    assert!(json.contains("AAEC"));
    assert!(json.contains("/w=="));
}

#[test]
fn datetime_parser_normalizes_rfc3339_to_utc() {
    let parsed = parse_datetime_text("2026-08-30T18:00:00+08:00", "时间");
    assert_eq!(
        parsed.map(|value| value.map(|value| value.to_rfc3339())),
        Ok(Some("2026-08-30T10:00:00+00:00".into()))
    );
    assert!(parse_datetime_text("not-a-time", "时间").is_err());
}

struct FakeStorage {
    cluster: KafkaClusterConfig,
}

#[async_trait]
impl Storage for FakeStorage {
    async fn list_kafka_clusters(&self) -> Result<Vec<KafkaClusterConfig>> {
        Ok(vec![self.cluster.clone()])
    }

    async fn list_connections(&self) -> Result<Vec<ConnectionConfig>> {
        Ok(Vec::new())
    }

    async fn get_connection(&self, _id: &ConnectionId) -> Result<Option<ConnectionConfig>> {
        Ok(None)
    }

    async fn save_connection(&self, _config: &ConnectionConfig) -> Result<()> {
        Ok(())
    }

    async fn delete_connection(&self, _id: &ConnectionId) -> Result<()> {
        Ok(())
    }

    async fn append_history(&self, _record: &QueryRecord) -> Result<()> {
        Ok(())
    }

    async fn list_history(
        &self,
        _connection_id: Option<&ConnectionId>,
        _limit: usize,
    ) -> Result<Vec<QueryRecord>> {
        Ok(Vec::new())
    }

    async fn delete_history(&self, _id: &QueryRecordId) -> Result<()> {
        Ok(())
    }

    async fn clear_history(&self, _connection_id: Option<&ConnectionId>) -> Result<()> {
        Ok(())
    }

    async fn get_preference(&self, _key: &str) -> Result<Option<String>> {
        Ok(None)
    }

    async fn set_preference(&self, _key: &str, _value: &str) -> Result<()> {
        Ok(())
    }
}

struct FakeKafkaDriver;

#[async_trait]
impl KafkaDriver for FakeKafkaDriver {
    async fn test_connection(&self, _config: &KafkaClusterConfig) -> Result<()> {
        Ok(())
    }

    async fn cluster_metadata(&self, _config: &KafkaClusterConfig) -> Result<KafkaClusterMetadata> {
        Ok(KafkaClusterMetadata {
            cluster_id: Some("test-cluster".into()),
            controller_id: Some(0),
            brokers: vec![KafkaBroker {
                id: 0,
                host: "127.0.0.1".into(),
                port: 19092,
                rack: None,
                version: Some("4.0.0".into()),
                is_controller: true,
            }],
            kafka_version: Some("4.0.0".into()),
        })
    }

    async fn list_topics(&self, _config: &KafkaClusterConfig) -> Result<Vec<KafkaTopic>> {
        Ok(vec![KafkaTopic {
            name: "ramag.integration.messages".into(),
            internal: false,
            partitions: (0..3)
                .map(|id| KafkaPartition {
                    id,
                    leader: Some(0),
                    replicas: vec![0],
                    isr: vec![0],
                    low_watermark: Some(0),
                    high_watermark: Some(1),
                })
                .collect(),
        }])
    }

    async fn read_messages(
        &self,
        _config: &KafkaClusterConfig,
        _query: &KafkaMessageQuery,
    ) -> Result<KafkaMessagePage> {
        Ok(KafkaMessagePage::empty())
    }

    async fn search_messages(
        &self,
        _config: &KafkaClusterConfig,
        _query: &KafkaMessageSearchQuery,
    ) -> Result<KafkaMessagePage> {
        Ok(KafkaMessagePage::empty())
    }
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx.debug_bounds(selector);
    assert!(bounds.is_some(), "控件应参与布局: {selector}");
    let Some(bounds) = bounds else {
        return;
    };
    let center = point(
        bounds.origin.x + bounds.size.width / 2.0,
        bounds.origin.y + bounds.size.height / 2.0,
    );
    cx.simulate_mouse_down(center, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(center, MouseButton::Left, Modifiers::default());
}

#[gpui::test]
fn kafka_workspace_renders_real_data_and_cancel_control(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let cluster = KafkaClusterConfig::new("Docker Kafka", vec!["127.0.0.1:19092".into()]);
    let service = Arc::new(KafkaService::new(
        Arc::new(FakeKafkaDriver),
        Arc::new(FakeStorage {
            cluster: cluster.clone(),
        }),
    ));
    let mut kafka_entity = None;
    let (_, visual_cx) = cx.add_window_view(|window, cx| {
        let kafka = cx.new(|cx| KafkaView::new(service, window, cx));
        kafka_entity = Some(kafka.clone());
        gpui_component::Root::new(kafka, window, cx)
    });
    assert!(kafka_entity.is_some(), "Kafka 视图实体应创建");
    let Some(kafka_entity) = kafka_entity else {
        return;
    };
    visual_cx.simulate_resize(size(px(1200.0), px(780.0)));
    visual_cx.run_until_parked();
    assert!(visual_cx.debug_bounds("kafka-root").is_some());
    assert!(visual_cx.debug_bounds("kafka-cluster-row").is_some());
    let status_bounds = visual_cx.debug_bounds("kafka-header-status");
    assert!(
        status_bounds.is_some(),
        "Kafka header status should participate in layout"
    );
    let Some(status_bounds) = status_bounds else {
        return;
    };
    assert!(
        status_bounds.size.width > px(140.0),
        "Kafka header status collapsed to {:?}",
        status_bounds.size
    );
    kafka_entity.update(visual_cx, |view, cx| {
        view.selected_cluster_id = Some(cluster.id.clone());
        view.metadata = Some(KafkaClusterMetadata {
            cluster_id: Some("test-cluster".into()),
            controller_id: Some(0),
            brokers: vec![KafkaBroker {
                id: 0,
                host: "127.0.0.1".into(),
                port: 19092,
                rack: None,
                version: Some("4.0.0".into()),
                is_controller: true,
            }],
            kafka_version: Some("4.0.0".into()),
        });
        view.topics = vec![KafkaTopic {
            name: "ramag.integration.messages".into(),
            internal: false,
            partitions: vec![KafkaPartition {
                id: 0,
                leader: Some(0),
                replicas: vec![0],
                isr: vec![0],
                low_watermark: Some(0),
                high_watermark: Some(1),
            }],
        }];
        view.loading_runtime = false;
        cx.notify();
    });
    visual_cx.run_until_parked();
    assert!(visual_cx.debug_bounds("kafka-overview-scroll").is_some());
    click(visual_cx, "kafka-section-Messages");
    visual_cx.run_until_parked();
    assert!(visual_cx.debug_bounds("kafka-messages").is_some());
    assert!(visual_cx.debug_bounds("kafka-read-messages").is_some());

    kafka_entity.update(visual_cx, |view, cx| {
        view.loading_messages = true;
        cx.notify();
    });
    visual_cx.run_until_parked();
    click(visual_cx, "kafka-cancel-messages");
    assert!(!kafka_entity.read_with(visual_cx, |view, _| view.loading_messages));
}
