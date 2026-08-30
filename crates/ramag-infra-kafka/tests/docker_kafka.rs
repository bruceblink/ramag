#![cfg(feature = "cmake-build")]

use ramag_domain::entities::KafkaClusterConfig;
use ramag_domain::traits::KafkaDriver;
use ramag_infra_kafka::RdkafkaDriver;
use std::env;

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
