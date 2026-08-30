use chrono::{TimeZone, Utc};

use super::kafka::{KafkaTopic, *};

fn valid_cluster() -> KafkaClusterConfig {
    KafkaClusterConfig::new("local", vec!["127.0.0.1:9092".into()])
}

fn valid_partition(id: i32) -> KafkaPartition {
    KafkaPartition {
        id,
        leader: Some(1),
        replicas: vec![1, 2],
        isr: vec![1, 2],
        low_watermark: Some(0),
        high_watermark: Some(10),
    }
}

#[test]
fn cluster_defaults_to_read_only_and_validates_bootstrap_servers() {
    let config = valid_cluster();

    assert_eq!(config.read_only, KafkaReadOnlyState::ReadOnly);
    assert!(!config.uses_tls());
    assert!(!config.uses_sasl());
    assert!(config.validate().is_ok());
    assert!(validate_kafka_bootstrap_server("[::1]:9092").is_ok());
    assert!(validate_kafka_bootstrap_server("localhost").is_err());
    assert!(validate_kafka_bootstrap_server("localhost:0").is_err());
    assert!(validate_kafka_bootstrap_server("kafka:9092,other:9092").is_err());
}

#[test]
fn cluster_rejects_duplicate_servers_and_incompatible_security_options() {
    let mut config = valid_cluster();
    config.bootstrap_servers.push("127.0.0.1:9092".into());
    assert!(config.validate().is_err());

    config = valid_cluster();
    config.sasl_mechanism = Some(KafkaSaslMechanism::Plain);
    assert!(config.validate().is_err());

    config = valid_cluster();
    config.security_protocol = KafkaSecurityProtocol::SaslSsl;
    config.sasl_mechanism = Some(KafkaSaslMechanism::ScramSha256);
    config.sasl_username = Some("user".into());
    config.sasl_password = Some("password".into());
    config.tls.ca_cert_path = Some("ca.pem".into());
    assert!(config.validate().is_ok());
    assert!(config.uses_tls());
    assert!(config.uses_sasl());
}

#[test]
fn cluster_rejects_oversized_text_and_non_tls_certificate_paths() {
    let mut config = valid_cluster();
    config.name = "n".repeat(MAX_KAFKA_CLUSTER_NAME_BYTES + 1);
    assert!(config.validate().is_err());

    config = valid_cluster();
    config.tls.ca_cert_path = Some("ca.pem".into());
    assert!(config.validate().is_err());

    config = valid_cluster();
    config.security_protocol = KafkaSecurityProtocol::SaslPlaintext;
    config.sasl_mechanism = Some(KafkaSaslMechanism::Plain);
    config.sasl_password = Some("bad\0secret".into());
    assert!(config.validate().is_err());
}

#[test]
fn cluster_debug_output_redacts_credentials_and_certificate_paths() {
    let mut config = valid_cluster();
    config.security_protocol = KafkaSecurityProtocol::SaslSsl;
    config.sasl_mechanism = Some(KafkaSaslMechanism::ScramSha256);
    config.sasl_username = Some("secret-user".into());
    config.sasl_password = Some("secret-password".into());
    config.tls.ca_cert_path = Some("C:\\private\\ca.pem".into());
    config.tls.client_cert_path = Some("C:\\private\\client.pem".into());
    config.tls.client_key_path = Some("C:\\private\\client.key".into());

    let rendered = format!("{config:?}");
    assert!(!rendered.contains("secret-user"));
    assert!(!rendered.contains("secret-password"));
    assert!(!rendered.contains("C:\\private"));
    assert!(rendered.contains("[REDACTED]"));
}

#[test]
fn password_based_sasl_requires_both_credentials() {
    let mut config = valid_cluster();
    config.security_protocol = KafkaSecurityProtocol::SaslPlaintext;
    config.sasl_mechanism = Some(KafkaSaslMechanism::Plain);
    config.sasl_username = Some("user".into());
    assert!(config.validate().is_err());

    config.sasl_password = Some("password".into());
    assert!(config.validate().is_ok());
}

#[test]
fn metadata_and_topics_reject_duplicate_or_invalid_entries() {
    let broker = KafkaBroker {
        id: 1,
        host: "broker-1".into(),
        port: 9092,
        rack: None,
        version: Some("3.8.0".into()),
        is_controller: true,
    };
    let metadata = KafkaClusterMetadata {
        cluster_id: Some("cluster-a".into()),
        controller_id: Some(1),
        brokers: vec![broker.clone()],
        kafka_version: Some("3.8.0".into()),
    };
    assert!(metadata.validate().is_ok());

    let duplicate = KafkaClusterMetadata {
        brokers: vec![broker.clone(), broker],
        ..metadata
    };
    assert!(duplicate.validate().is_err());

    let topic = KafkaTopic {
        name: "events".into(),
        partitions: vec![valid_partition(0), valid_partition(1)],
        internal: false,
    };
    assert!(topic.validate().is_ok());

    let invalid_topic = KafkaTopic {
        name: "..".into(),
        ..topic
    };
    assert!(invalid_topic.validate().is_err());
}

#[test]
fn partition_checks_replica_relationships_and_watermarks() {
    let mut partition = valid_partition(0);
    assert!(partition.validate().is_ok());

    partition.isr = vec![3];
    assert!(partition.validate().is_err());

    partition = valid_partition(0);
    partition.high_watermark = Some(0);
    partition.low_watermark = Some(1);
    assert!(partition.validate().is_err());
}

#[test]
fn message_keeps_raw_bytes_and_bounds_previews() {
    let message = KafkaMessageRecord {
        topic: "events".into(),
        partition: 0,
        offset: 42,
        timestamp: None,
        key: Some(vec![0xff, b'a']),
        value: Some("你好世界".as_bytes().to_vec()),
        headers: vec![KafkaMessageHeader {
            key: "trace-id".into(),
            value: Some(vec![1, 2, 3]),
        }],
    };

    assert!(message.validate().is_ok());
    assert_eq!(message.key.as_deref(), Some(&[0xff, b'a'][..]));
    assert_eq!(
        message.value_preview(4).map(|preview| preview.text),
        Some("你".into())
    );
    assert!(
        message
            .value_preview(4)
            .is_some_and(|preview| preview.truncated)
    );
    assert!(message.retained_bytes() >= 6);
}

#[test]
fn message_preview_does_not_split_utf8_and_handles_empty_values() {
    let preview = preview_bytes("你好".as_bytes(), 4);
    assert_eq!(preview.text, "你");
    assert!(preview.truncated);
    assert_eq!(
        preview_bytes(&[], 0),
        KafkaTextPreview {
            text: String::new(),
            truncated: false
        }
    );
    assert_eq!(
        preview_bytes(b"abc", 0),
        KafkaTextPreview {
            text: String::new(),
            truncated: true
        }
    );
    assert!(preview_bytes(&[0xff], 1).truncated);
}

#[test]
fn offset_and_time_queries_require_one_bounded_range() {
    let query = KafkaMessageQuery::by_offset("events", vec![0, 1], 10, Some(20));
    assert!(query.validate().is_ok());

    let invalid = query.clone().with_limits(0, DEFAULT_KAFKA_MAX_BYTES, 30, 4);
    assert!(invalid.validate().is_err());

    let invalid_range = KafkaMessageQuery::by_offset("events", vec![0], 20, Some(20));
    assert!(invalid_range.validate().is_err());

    let no_range = KafkaMessageQuery::by_offset("events", vec![0], 0, None);
    assert!(no_range.validate().is_ok());

    let time = Utc.with_ymd_and_hms(2026, 8, 30, 10, 0, 0).single();
    let Some(time) = time else { return };
    let time_query = KafkaMessageQuery::by_time("events", vec![0], time, None);
    assert!(time_query.validate().is_ok());
}

#[test]
fn queries_reject_duplicate_partitions_and_mixed_ranges() {
    let mut query = KafkaMessageQuery::by_offset("events", vec![0, 0], 0, None);
    assert!(query.validate().is_err());

    query = KafkaMessageQuery::by_offset("events", vec![0], 0, None);
    query.start_time = Some(Utc::now());
    assert!(query.validate().is_err());
}

#[test]
fn search_query_requires_non_duplicate_fields_and_non_empty_text() {
    let scan = KafkaMessageQuery::by_offset("events", vec![0], 0, Some(10));
    let query = KafkaMessageSearchQuery::new("error", scan.clone());
    assert!(query.validate().is_ok());

    let duplicate = KafkaMessageSearchQuery::new("error", scan.clone()).with_fields(vec![
        KafkaMessageSearchField::Value,
        KafkaMessageSearchField::Value,
    ]);
    assert!(duplicate.validate().is_err());

    let empty = KafkaMessageSearchQuery::new("", scan);
    assert!(empty.validate().is_err());
}

#[test]
fn consumer_groups_validate_members_assignments_and_offsets() {
    let group = KafkaConsumerGroup {
        group_id: "workers".into(),
        state: Some("Stable".into()),
        protocol: Some("range".into()),
        members: vec![KafkaConsumerMember {
            member_id: "member-1".into(),
            client_id: "worker".into(),
            client_host: Some("/127.0.0.1".into()),
            assigned_partitions: vec![KafkaConsumerPartitionAssignment {
                topic: "events".into(),
                partition: 0,
            }],
        }],
        offsets: vec![KafkaConsumerGroupOffset {
            topic: "events".into(),
            partition: 0,
            committed_offset: Some(8),
            end_offset: Some(10),
            lag: Some(2),
        }],
    };
    assert!(group.validate().is_ok());

    let invalid = KafkaConsumerGroupOffset {
        committed_offset: Some(11),
        ..group.offsets[0].clone()
    };
    assert!(invalid.validate().is_err());
}

#[test]
fn acl_validation_requires_exact_non_empty_target_fields() {
    let acl = KafkaAcl {
        principal: "User:app".into(),
        host: "*".into(),
        resource_type: KafkaAclResourceType::Topic,
        resource_name: "events".into(),
        pattern_type: KafkaAclPatternType::Literal,
        operation: KafkaAclOperation::Read,
        permission: KafkaAclPermission::Allow,
    };
    assert!(acl.validate().is_ok());

    let invalid = KafkaAcl {
        resource_name: String::new(),
        ..acl
    };
    assert!(invalid.validate().is_err());
}

#[test]
fn serde_defaults_keep_new_optional_metadata_compatible() -> Result<(), serde_json::Error> {
    let config = serde_json::from_str::<KafkaClusterConfig>(
        r#"{"id":"00000000-0000-0000-0000-000000000000","name":"local","bootstrap_servers":["localhost:9092"]}"#,
    )?;
    assert_eq!(config.security_protocol, KafkaSecurityProtocol::Plaintext);
    assert_eq!(config.tls, KafkaTlsConfig::default());
    assert_eq!(config.read_only, KafkaReadOnlyState::ReadOnly);
    Ok(())
}
