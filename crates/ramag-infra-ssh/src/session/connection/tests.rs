use super::*;
use tokio::io::AsyncWriteExt as _;

#[test]
fn stderr_tail_is_bounded() {
    let mut tail = VecDeque::new();
    append_tail(&mut tail, &vec![b'a'; STDERR_LIMIT + 100]);
    assert_eq!(tail.len(), STDERR_LIMIT);
}

#[test]
fn connection_errors_explain_auth_and_host_key_limits() {
    let host_key = VecDeque::from(b"Host key verification failed".to_vec());
    assert!(
        connection_error("bad".into(), &host_key)
            .message()
            .contains("known_hosts")
    );
    let auth = VecDeque::from(b"Permission denied (publickey,password)".to_vec());
    assert!(
        connection_error("bad".into(), &auth)
            .message()
            .contains("认证失败")
    );
}

#[test]
fn transfer_chunks_respect_server_and_local_limits() {
    assert_eq!(bounded_chunk(None), 64 * 1024);
    assert_eq!(bounded_chunk(Some(4096)), 4096);
    assert_eq!(bounded_chunk(Some(0)), 1);
    assert_eq!(bounded_chunk(Some(u64::MAX)), 64 * 1024);
}

#[tokio::test]
async fn packet_relay_forwards_valid_frame_and_rejects_oversized_frame() {
    let (mut input, input_reader) = tokio::io::duplex(64);
    let (mut output_reader, output) = tokio::io::duplex(64);
    let relay = tokio::spawn(relay_bounded_packets(
        input_reader,
        output,
        false,
        "test".into(),
    ));
    input.write_all(&3u32.to_be_bytes()).await.unwrap();
    input.write_all(&[1, 2, 3]).await.unwrap();
    input.shutdown().await.unwrap();
    let mut forwarded = Vec::new();
    output_reader.read_to_end(&mut forwarded).await.unwrap();
    relay.await.unwrap();
    assert_eq!(forwarded, [0, 0, 0, 3, 1, 2, 3]);

    let (mut input, input_reader) = tokio::io::duplex(64);
    let (mut output_reader, output) = tokio::io::duplex(64);
    let relay = tokio::spawn(relay_bounded_packets(
        input_reader,
        output,
        false,
        "test".into(),
    ));
    input
        .write_all(&(MAX_SFTP_PACKET_BYTES + 1).to_be_bytes())
        .await
        .unwrap();
    input.shutdown().await.unwrap();
    let mut forwarded = Vec::new();
    output_reader.read_to_end(&mut forwarded).await.unwrap();
    relay.await.unwrap();
    assert!(forwarded.is_empty());
}

#[tokio::test]
async fn packet_relay_skips_a_bounded_jumpserver_banner_before_version() {
    let (mut input, input_reader) = tokio::io::duplex(256);
    let (mut output_reader, output) = tokio::io::duplex(256);
    let relay = tokio::spawn(relay_bounded_packets(
        input_reader,
        output,
        true,
        "test".into(),
    ));
    input
        .write_all(b"Welcome to JumpServer SSH Server\r\n")
        .await
        .unwrap();
    input.write_all(&5u32.to_be_bytes()).await.unwrap();
    input.write_all(&[2, 0, 0, 0, 3]).await.unwrap();
    input.shutdown().await.unwrap();
    let mut forwarded = Vec::new();
    output_reader.read_to_end(&mut forwarded).await.unwrap();
    relay.await.unwrap();

    assert_eq!(forwarded, [0, 0, 0, 5, 2, 0, 0, 0, 3]);
}
