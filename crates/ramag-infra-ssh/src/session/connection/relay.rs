use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::{MAX_SFTP_PACKET_BYTES, TEXT_PREAMBLE_LIMIT};

pub(super) async fn relay_bounded_packets<R, W>(
    mut stdout: R,
    mut destination: W,
    allow_text_preamble: bool,
    profile_id: String,
) where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let Some((header, first_body_byte)) =
        read_first_packet_header(&mut stdout, allow_text_preamble, &profile_id).await
    else {
        if let Err(error) = destination.shutdown().await {
            tracing::warn!(operation = "ssh_sftp_relay_shutdown", profile_id, stage = "missing_packet", error = %error, "shutdown ssh sftp relay after missing packet failed");
        }
        return;
    };
    let mut buffer = [0u8; 32 * 1024];
    if let Err(error) = relay_packet(
        &mut stdout,
        &mut destination,
        header,
        first_body_byte,
        &mut buffer,
        &profile_id,
    )
    .await
    {
        tracing::warn!(operation = "ssh_sftp_relay", profile_id, stage = "first_packet", error = %error, "relay ssh sftp first packet failed");
        if let Err(shutdown_error) = destination.shutdown().await {
            tracing::warn!(operation = "ssh_sftp_relay_shutdown", profile_id, stage = "packet_failure", error = %shutdown_error, "shutdown ssh sftp relay after packet failure failed");
        }
        return;
    }
    loop {
        let mut header = [0u8; 4];
        match stdout.read_exact(&mut header).await {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => {
                tracing::warn!(operation = "ssh_sftp_relay", profile_id, stage = "packet_header", error = %error, "read ssh sftp packet header failed");
                break;
            }
        }
        if let Err(error) = relay_packet(
            &mut stdout,
            &mut destination,
            header,
            None,
            &mut buffer,
            &profile_id,
        )
        .await
        {
            tracing::warn!(operation = "ssh_sftp_relay", profile_id, stage = "packet", error = %error, "relay ssh sftp packet failed");
            break;
        }
    }
    if let Err(error) = destination.shutdown().await {
        tracing::warn!(operation = "ssh_sftp_relay_shutdown", profile_id, error = %error, "shutdown ssh sftp protocol relay failed");
    }
}

async fn read_first_packet_header<R>(
    stdout: &mut R,
    allow_text_preamble: bool,
    profile_id: &str,
) -> Option<([u8; 4], Option<u8>)>
where
    R: tokio::io::AsyncRead + Unpin,
{
    if !allow_text_preamble {
        let mut header = [0u8; 4];
        return stdout
            .read_exact(&mut header)
            .await
            .ok()
            .map(|_| (header, None));
    }
    let mut scanned = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while scanned.len() < TEXT_PREAMBLE_LIMIT {
        if let Err(error) = stdout.read_exact(&mut byte).await {
            if error.kind() != std::io::ErrorKind::UnexpectedEof {
                tracing::warn!(operation = "ssh_sftp_relay", profile_id, stage = "first_packet", error = %error, "read ssh sftp first packet failed");
            }
            return None;
        }
        scanned.push(byte[0]);
        if scanned.len() < 5 {
            continue;
        }
        let start = scanned.len() - 5;
        let header: [u8; 4] = scanned[start..start + 4].try_into().ok()?;
        let packet_bytes = u32::from_be_bytes(header);
        // SFTP 首包必须是至少含类型和版本号的 SSH_FXP_VERSION。
        if (5..=MAX_SFTP_PACKET_BYTES).contains(&packet_bytes) && scanned[start + 4] == 2 {
            if start > 0 {
                tracing::info!(
                    operation = "ssh_sftp_relay",
                    profile_id,
                    stage = "jumpserver_preamble",
                    bytes = start,
                    "ignored jumpserver sftp text preamble"
                );
            }
            return Some((header, Some(2)));
        }
    }
    tracing::warn!(
        operation = "ssh_sftp_relay",
        profile_id,
        stage = "jumpserver_preamble_limit",
        "jumpserver sftp text preamble exceeded safety limit"
    );
    None
}

async fn relay_packet<R, W>(
    stdout: &mut R,
    destination: &mut W,
    header: [u8; 4],
    first_body_byte: Option<u8>,
    buffer: &mut [u8],
    profile_id: &str,
) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let packet_bytes = u32::from_be_bytes(header);
    if packet_bytes == 0 || packet_bytes > MAX_SFTP_PACKET_BYTES {
        tracing::warn!(
            operation = "ssh_sftp_relay",
            profile_id,
            stage = "packet_limit",
            packet_bytes,
            "ssh sftp packet exceeded safety limit"
        );
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid sftp packet size",
        ));
    }
    destination.write_all(&header).await?;
    let mut remaining = packet_bytes as usize;
    if let Some(first) = first_body_byte {
        destination.write_all(&[first]).await?;
        remaining = remaining.saturating_sub(1);
    }
    while remaining > 0 {
        let chunk = remaining.min(buffer.len());
        stdout.read_exact(&mut buffer[..chunk]).await?;
        destination.write_all(&buffer[..chunk]).await?;
        remaining -= chunk;
    }
    Ok(())
}
