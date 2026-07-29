//! 真实 OpenSSH/SFTP 集成测试。
//!
//! 未配置 `RAMAG_TEST_SSH_HOST` 时跳过；测试目录必须由
//! `RAMAG_TEST_SSH_ROOT` 指向名称含 `ramag` 的专用绝对目录。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_compression::futures::bufread::GzipDecoder;
use futures::io::{AsyncReadExt as _, BufReader};
use futures::stream::StreamExt as _;
use ramag_domain::entities::{
    OverwritePolicy, RemoteEntryKind, RemoteFileChunkPosition, SshAuthMode, SshProfile,
    SshProgressFn, TransferCancellation, join_remote_path, validate_remote_path,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::SshDriver;
use ramag_infra_ssh::OpenSshDriver;

struct Fixture {
    profile: SshProfile,
    root: String,
}

fn fixture_from_env() -> Result<Option<Fixture>> {
    let Some(host) = std::env::var("RAMAG_TEST_SSH_HOST").ok() else {
        return Ok(None);
    };
    let root = std::env::var("RAMAG_TEST_SSH_ROOT").map_err(|_| {
        DomainError::InvalidConfig(
            "设置 RAMAG_TEST_SSH_HOST 时也必须设置 RAMAG_TEST_SSH_ROOT".into(),
        )
    })?;
    validate_remote_path(&root).map_err(DomainError::InvalidConfig)?;
    if !root.starts_with('/') || root == "/" || !root.to_ascii_lowercase().contains("ramag") {
        return Err(DomainError::InvalidConfig(
            "RAMAG_TEST_SSH_ROOT 必须是名称含 ramag 的专用绝对目录，且不能是根目录".into(),
        ));
    }

    let mut profile = SshProfile::new("integration-test", host);
    if let Ok(port) = std::env::var("RAMAG_TEST_SSH_PORT") {
        profile.port = Some(port.parse::<u16>().map_err(|error| {
            DomainError::InvalidConfig(format!("RAMAG_TEST_SSH_PORT 无效：{error}"))
        })?);
    }
    profile.username = std::env::var("RAMAG_TEST_SSH_USER").unwrap_or_default();
    if let Ok(key_path) = std::env::var("RAMAG_TEST_SSH_KEY_PATH") {
        profile.auth_mode = SshAuthMode::KeyFile;
        profile.key_path = Some(key_path);
    }
    profile.ssh_path = std::env::var("RAMAG_TEST_SSH_PATH").ok();
    profile.initial_directory = Some(root.clone());
    profile.validate().map_err(DomainError::InvalidConfig)?;
    Ok(Some(Fixture { profile, root }))
}

#[tokio::test]
async fn openssh_sftp_round_trip_is_streamed_and_cleaned() -> Result<()> {
    let Some(fixture) = fixture_from_env()? else {
        return Ok(());
    };
    let driver = OpenSshDriver::new();
    let case_name = format!("case-{}", uuid::Uuid::new_v4());
    let case_directory =
        join_remote_path(&fixture.root, &case_name).map_err(DomainError::InvalidConfig)?;
    let source_path =
        join_remote_path(&case_directory, "source.bin").map_err(DomainError::InvalidConfig)?;
    let renamed_path =
        join_remote_path(&case_directory, "renamed.bin").map_err(DomainError::InvalidConfig)?;
    let local_directory = tempfile::tempdir()
        .map_err(|error| DomainError::Other(format!("创建本地测试目录失败：{error}")))?;
    let local_source = local_directory.path().join("source.bin");
    let local_download = local_directory.path().join("download.bin");
    let local_archive = local_directory.path().join("case.tar.gz");
    let payload = (0..256 * 1024)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let edited = b"edited through Ramag\n".to_vec();
    std::fs::write(&local_source, &payload)
        .map_err(|error| DomainError::Other(format!("写入本地测试文件失败：{error}")))?;
    let uploaded = Arc::new(AtomicU64::new(0));
    let uploaded_for_progress = uploaded.clone();
    let upload_progress: SshProgressFn = Arc::new(move |transferred, _| {
        uploaded_for_progress.store(transferred, Ordering::Release);
    });

    let result = async {
        driver.probe(fixture.profile.ssh_path.as_deref()).await?;
        driver.test_connection(&fixture.profile).await?;
        driver
            .create_directory(&fixture.profile, &case_directory)
            .await?;
        driver
            .upload(
                &fixture.profile,
                &local_source,
                &source_path,
                OverwritePolicy::Refuse,
                TransferCancellation::default(),
                upload_progress,
            )
            .await?;
        let entries = driver
            .list_directory(&fixture.profile, &case_directory)
            .await?;
        if !entries
            .entries
            .iter()
            .any(|entry| entry.path == source_path && entry.size == payload.len() as u64)
        {
            return Err(DomainError::Other(
                "上传后的远程文件元数据不符合预期".into(),
            ));
        }
        let preview = driver
            .read_file_preview(&fixture.profile, &source_path)
            .await?;
        if preview.bytes != payload || preview.truncated {
            return Err(DomainError::Other("远程文件预览内容不符合预期".into()));
        }
        let chunk = driver
            .read_file_chunk(
                &fixture.profile,
                &source_path,
                RemoteFileChunkPosition::Tail,
            )
            .await?;
        if chunk.bytes != payload || chunk.offset != 0 || chunk.total_bytes != payload.len() as u64
        {
            return Err(DomainError::Other("远程文件分段内容不符合预期".into()));
        }
        driver
            .save_file(&fixture.profile, &source_path, &payload, &edited)
            .await?;
        driver
            .download(
                &fixture.profile,
                &source_path,
                &local_download,
                OverwritePolicy::Refuse,
                TransferCancellation::default(),
                Arc::new(|_, _| {}),
            )
            .await?;
        let downloaded = std::fs::read(&local_download)
            .map_err(|error| DomainError::Other(format!("读取下载结果失败：{error}")))?;
        if downloaded != edited {
            return Err(DomainError::Other("下载内容与上传源不一致".into()));
        }
        driver
            .rename(&fixture.profile, &source_path, &renamed_path)
            .await?;
        let archived = Arc::new(AtomicU64::new(0));
        let archived_for_progress = archived.clone();
        driver
            .download_directory(
                &fixture.profile,
                &case_directory,
                &local_archive,
                OverwritePolicy::Refuse,
                TransferCancellation::default(),
                Arc::new(move |transferred, _| {
                    archived_for_progress.store(transferred, Ordering::Release);
                }),
            )
            .await?;
        let archive_size = std::fs::metadata(&local_archive)
            .map_err(|error| DomainError::Other(format!("读取目录归档信息失败：{error}")))?
            .len();
        if archive_size == 0 || archived.load(Ordering::Acquire) != edited.len() as u64 {
            return Err(DomainError::Other("目录归档结果不符合预期".into()));
        }
        let archive_file = async_std::fs::File::open(&local_archive)
            .await
            .map_err(|error| DomainError::Other(format!("打开目录归档失败：{error}")))?;
        let mut archive = async_tar::Archive::new(GzipDecoder::new(BufReader::new(archive_file)))
            .entries()
            .map_err(|error| DomainError::Other(format!("读取目录归档失败：{error}")))?;
        let expected_archive_path = format!("{case_name}/renamed.bin");
        let mut archived_contents = None;
        while let Some(entry) = archive.next().await {
            let mut entry = entry
                .map_err(|error| DomainError::Other(format!("读取目录归档项目失败：{error}")))?;
            let path = entry
                .path()
                .map_err(|error| DomainError::Other(format!("读取归档路径失败：{error}")))?;
            if path.to_string_lossy() == expected_archive_path {
                let mut contents = Vec::new();
                entry
                    .read_to_end(&mut contents)
                    .await
                    .map_err(|error| DomainError::Other(format!("读取归档文件失败：{error}")))?;
                archived_contents = Some(contents);
            }
        }
        if archived_contents.as_deref() != Some(edited.as_slice()) {
            return Err(DomainError::Other("目录归档文件内容不符合预期".into()));
        }
        driver
            .remove(&fixture.profile, &renamed_path, RemoteEntryKind::File)
            .await?;
        Ok(())
    }
    .await;

    let cleanup = driver
        .remove(
            &fixture.profile,
            &case_directory,
            RemoteEntryKind::Directory,
        )
        .await;
    let shutdown = driver.shutdown().await;
    result?;
    cleanup?;
    shutdown?;
    if uploaded.load(Ordering::Acquire) != payload.len() as u64 {
        return Err(DomainError::Other("上传进度未到达文件总大小".into()));
    }
    Ok(())
}
