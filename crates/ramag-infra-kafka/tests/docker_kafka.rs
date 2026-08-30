#![cfg(feature = "cmake-build")]

use chrono::{Duration, Utc};
use ramag_domain::entities::{
    KafkaClusterConfig, KafkaMessageQuery, KafkaMessageSearchField, KafkaMessageSearchQuery,
};
use ramag_domain::traits::KafkaDriver;
use ramag_infra_kafka::RdkafkaDriver;
use std::env;

const FIXTURE_TOPIC: &str = "ramag.integration.messages";

/// Connects to the broker started by the Docker test runner and requests metadata.
#[test]
fn docker_kafka_accepts_metadata_request() {
    let bootstrap = env::var("RAMAG_TEST_KAFKA_BOOTSTRAP").unwrap_or_default();
    let config = KafkaClusterConfig::new("ramag-docker-kafka", vec![bootstrap]);
    let driver = RdkafkaDriver::new();

    let result = smol::block_on(driver.test_connection(&config));
    assert!(
        result.is_ok(),
        "Docker Kafka metadata request failed; run scripts/kafka-test/kafka-test.ps1 up first: {result:?}"
    );
}

/// Reads the seeded fixture through the production driver, including metadata, watermarks,
/// offset ranges, time ranges, bounded search, and the no-commit read path.
#[test]
fn docker_kafka_reads_and_searches_seeded_fixture() {
    let bootstrap = env::var("RAMAG_TEST_KAFKA_BOOTSTRAP").unwrap_or_default();
    let config = KafkaClusterConfig::new("ramag-docker-kafka", vec![bootstrap]);
    let driver = RdkafkaDriver::new();

    let metadata_result = smol::block_on(driver.cluster_metadata(&config));
    assert!(
        metadata_result.is_ok(),
        "Docker Kafka cluster metadata should be available: {metadata_result:?}"
    );
    let Ok(metadata) = metadata_result else {
        return;
    };
    assert!(!metadata.brokers.is_empty());

    let topics_result = smol::block_on(driver.list_topics(&config));
    assert!(
        topics_result.is_ok(),
        "Docker Kafka topic metadata should be available: {topics_result:?}"
    );
    let Ok(topics) = topics_result else {
        return;
    };
    let fixture = topics.iter().find(|topic| topic.name == FIXTURE_TOPIC);
    assert!(fixture.is_some(), "the Docker fixture topic should exist");
    let Some(fixture) = fixture else {
        return;
    };
    assert_eq!(fixture.partitions.len(), 3);
    let total_messages = fixture
        .partitions
        .iter()
        .map(|partition| partition.high_watermark.unwrap_or_default())
        .sum::<i64>();
    assert!(
        total_messages >= 3,
        "the fixture topic should contain at least the three seeded messages"
    );

    let scan = KafkaMessageQuery::by_offset(FIXTURE_TOPIC, vec![0, 1, 2], 0, None).with_limits(
        10,
        1024 * 1024,
        30,
        3,
    );
    let page_result = smol::block_on(driver.read_messages(&config, &scan));
    assert!(
        page_result.is_ok(),
        "the Docker fixture should be readable by offset: {page_result:?}"
    );
    let Ok(page) = page_result else {
        return;
    };
    assert_eq!(
        page.records.len(),
        3,
        "all seeded messages should be returned"
    );
    assert_eq!(page.scanned_records, 3);
    assert!(page.records.iter().any(|record| {
        record.key.as_deref() == Some(b"event-002")
            && record
                .value
                .as_deref()
                .is_some_and(|value| value.windows(7).any(|part| part == b"updated"))
    }));

    let time_scan = KafkaMessageQuery::by_time(
        FIXTURE_TOPIC,
        vec![0, 1, 2],
        Utc::now() - Duration::minutes(5),
        Some(Utc::now() + Duration::minutes(5)),
    )
    .with_limits(10, 1024 * 1024, 30, 3);
    let time_page_result = smol::block_on(driver.read_messages(&config, &time_scan));
    assert!(
        time_page_result.is_ok(),
        "the Docker fixture should be readable by time: {time_page_result:?}"
    );
    let Ok(time_page) = time_page_result else {
        return;
    };
    assert_eq!(time_page.records.len(), 3);
    assert!(
        time_page
            .records
            .iter()
            .all(|record| record.timestamp.is_some())
    );

    let search = KafkaMessageSearchQuery::new("updated", scan.clone())
        .with_fields(vec![KafkaMessageSearchField::Value]);
    let search_page_result = smol::block_on(driver.search_messages(&config, &search));
    assert!(
        search_page_result.is_ok(),
        "the Docker fixture should support bounded value search: {search_page_result:?}"
    );
    let Ok(search_page) = search_page_result else {
        return;
    };
    assert_eq!(search_page.records.len(), 1);
    assert_eq!(
        search_page.records[0].key.as_deref(),
        Some(&b"event-002"[..])
    );

    let bounded = scan.with_limits(1, 1024 * 1024, 30, 3);
    let bounded_page_result = smol::block_on(driver.read_messages(&config, &bounded));
    assert!(
        bounded_page_result.is_ok(),
        "bounded fixture read should succeed: {bounded_page_result:?}"
    );
    let Ok(bounded_page) = bounded_page_result else {
        return;
    };
    assert!(bounded_page.records.len() <= 1);
    assert!(bounded_page.scanned_records <= 1);
}
