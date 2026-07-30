//! 远程文本文件片段的校验、UTF-8 边界修复与行数限制。

use ramag_domain::entities::{
    MAX_REMOTE_FILE_PREVIEW_BYTES, RemoteFileChunk, RemoteFileChunkPosition,
};

pub(super) const MAX_REMOTE_FILE_PREVIEW_LINES: usize = 50_000;

pub(super) struct RemoteFileText {
    pub text: String,
    pub total_bytes: u64,
    pub offset: u64,
    pub end_offset: u64,
}

impl RemoteFileText {
    pub fn is_windowed(&self) -> bool {
        self.offset > 0 || self.end_offset < self.total_bytes
    }
}

pub(super) fn decode_remote_file_chunk(
    chunk: RemoteFileChunk,
    position: RemoteFileChunkPosition,
) -> Result<RemoteFileText, String> {
    let (expected_start, expected_end) = chunk_range(position, chunk.total_bytes);
    let byte_len = u64::try_from(chunk.bytes.len()).map_err(|_| invalid_chunk())?;
    let raw_end = chunk
        .offset
        .checked_add(byte_len)
        .ok_or_else(invalid_chunk)?;
    if chunk.bytes.len() > MAX_REMOTE_FILE_PREVIEW_BYTES
        || chunk.offset != expected_start
        || raw_end != expected_end
        || raw_end > chunk.total_bytes
    {
        return Err(invalid_chunk());
    }

    let mut bytes = chunk.bytes;
    let leading = incomplete_utf8_prefix_len(&bytes, chunk.offset)?;
    if leading > 0 {
        bytes.drain(..leading);
    }
    let mut offset = chunk.offset + leading as u64;
    let mut end_offset = raw_end;
    let mut text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            let utf8_error = error.utf8_error();
            if raw_end == chunk.total_bytes || utf8_error.error_len().is_some() {
                return Err("非 UTF-8 文件请下载查看".into());
            }
            let valid_up_to = utf8_error.valid_up_to();
            let mut bytes = error.into_bytes();
            bytes.truncate(valid_up_to);
            end_offset = offset + valid_up_to as u64;
            String::from_utf8(bytes).map_err(|_| "非 UTF-8 文件请下载查看".to_string())?
        }
    };
    if contains_binary_control(&text) {
        return Err("二进制文件请下载查看".into());
    }

    match position {
        RemoteFileChunkPosition::From(_) => {
            if let Some(end) = prefix_line_end(&text) {
                text.truncate(end);
                end_offset = offset + end as u64;
            }
        }
        RemoteFileChunkPosition::Before(_) | RemoteFileChunkPosition::Tail => {
            if let Some(start) = suffix_line_start(&text) {
                text = text.split_off(start);
                offset += start as u64;
            }
        }
    }

    Ok(RemoteFileText {
        text,
        total_bytes: chunk.total_bytes,
        offset,
        end_offset,
    })
}

pub(super) fn text_line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.bytes().filter(|byte| *byte == b'\n').count() + usize::from(!text.ends_with('\n'))
}

pub(super) fn merge_remote_file_tail(
    current: &RemoteFileText,
    appended: RemoteFileText,
) -> Result<RemoteFileText, String> {
    let current_end = current
        .offset
        .checked_add(current.text.len() as u64)
        .ok_or_else(invalid_chunk)?;
    let appended_end = appended
        .offset
        .checked_add(appended.text.len() as u64)
        .ok_or_else(invalid_chunk)?;
    if current.end_offset != current.total_bytes
        || current_end != current.end_offset
        || appended.offset != current.end_offset
        || appended_end != appended.end_offset
        || appended.end_offset != appended.total_bytes
    {
        return Err(invalid_chunk());
    }

    let mut text = String::with_capacity(current.text.len().saturating_add(appended.text.len()));
    text.push_str(&current.text);
    text.push_str(&appended.text);
    let mut offset = current.offset;

    if text.len() > MAX_REMOTE_FILE_PREVIEW_BYTES {
        let mut start = text.len() - MAX_REMOTE_FILE_PREVIEW_BYTES;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        text.drain(..start);
        offset = offset.checked_add(start as u64).ok_or_else(invalid_chunk)?;
    }
    if let Some(start) = suffix_line_start(&text) {
        text.drain(..start);
        offset = offset.checked_add(start as u64).ok_or_else(invalid_chunk)?;
    }

    Ok(RemoteFileText {
        text,
        total_bytes: appended.total_bytes,
        offset,
        end_offset: appended.end_offset,
    })
}

fn chunk_range(position: RemoteFileChunkPosition, total: u64) -> (u64, u64) {
    let limit = MAX_REMOTE_FILE_PREVIEW_BYTES as u64;
    match position {
        RemoteFileChunkPosition::From(requested) => {
            let start = requested.min(total);
            (start, start.saturating_add(limit).min(total))
        }
        RemoteFileChunkPosition::Before(requested) => {
            let end = requested.min(total);
            (end.saturating_sub(limit), end)
        }
        RemoteFileChunkPosition::Tail => (total.saturating_sub(limit), total),
    }
}

fn incomplete_utf8_prefix_len(bytes: &[u8], offset: u64) -> Result<usize, String> {
    if offset == 0 {
        return Ok(0);
    }
    let leading = bytes
        .iter()
        .take(3)
        .take_while(|byte| (**byte & 0b1100_0000) == 0b1000_0000)
        .count();
    if bytes
        .get(leading)
        .is_some_and(|byte| (*byte & 0b1100_0000) == 0b1000_0000)
    {
        return Err("非 UTF-8 文件请下载查看".into());
    }
    Ok(leading)
}

fn contains_binary_control(text: &str) -> bool {
    text.as_bytes()
        .iter()
        .any(|byte| (*byte < b' ' && !matches!(*byte, b'\t' | b'\n' | b'\r')) || *byte == 0x7F)
}

fn prefix_line_end(text: &str) -> Option<usize> {
    if text_line_count(text) <= MAX_REMOTE_FILE_PREVIEW_LINES {
        return None;
    }
    text.bytes()
        .enumerate()
        .filter(|(_, byte)| *byte == b'\n')
        .nth(MAX_REMOTE_FILE_PREVIEW_LINES - 1)
        .map(|(index, _)| index + 1)
}

fn suffix_line_start(text: &str) -> Option<usize> {
    let line_count = text_line_count(text);
    if line_count <= MAX_REMOTE_FILE_PREVIEW_LINES {
        return None;
    }
    text.bytes()
        .enumerate()
        .filter(|(_, byte)| *byte == b'\n')
        .nth(line_count - MAX_REMOTE_FILE_PREVIEW_LINES - 1)
        .map(|(index, _)| index + 1)
}

fn invalid_chunk() -> String {
    "文件片段异常，请重试".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_tail_chunk_is_complete_and_editable() {
        let decoded = decode_remote_file_chunk(
            RemoteFileChunk {
                bytes: b"hello\nworld".to_vec(),
                offset: 0,
                total_bytes: 11,
            },
            RemoteFileChunkPosition::Tail,
        )
        .expect("small UTF-8 file should decode");

        assert_eq!(decoded.text, "hello\nworld");
        assert_eq!((decoded.offset, decoded.end_offset), (0, 11));
        assert!(!decoded.is_windowed());
    }

    #[test]
    fn chunk_repairs_only_split_utf8_boundaries() {
        let bytes = "甲乙".as_bytes();
        let decoded = decode_remote_file_chunk(
            RemoteFileChunk {
                bytes: bytes[1..].to_vec(),
                offset: 1,
                total_bytes: bytes.len() as u64,
            },
            RemoteFileChunkPosition::From(1),
        )
        .expect("leading continuation bytes should be skipped");
        assert_eq!(decoded.text, "乙");
        assert_eq!(decoded.offset, 3);

        let limit = MAX_REMOTE_FILE_PREVIEW_BYTES;
        let mut split_tail = vec![b'a'; limit - 2];
        split_tail.extend_from_slice(&[0xE4, 0xB8]);
        let decoded = decode_remote_file_chunk(
            RemoteFileChunk {
                bytes: split_tail,
                offset: 0,
                total_bytes: limit as u64 + 1,
            },
            RemoteFileChunkPosition::From(0),
        )
        .expect("trailing partial character should be deferred to the next chunk");
        assert_eq!(decoded.end_offset, limit as u64 - 2);
    }

    #[test]
    fn chunk_rejects_binary_invalid_utf8_and_inconsistent_ranges() {
        for bytes in [vec![b'a', 0, b'b'], vec![b'a', 0x7F], vec![0xFF]] {
            assert!(
                decode_remote_file_chunk(
                    RemoteFileChunk {
                        total_bytes: bytes.len() as u64,
                        offset: 0,
                        bytes,
                    },
                    RemoteFileChunkPosition::Tail,
                )
                .is_err()
            );
        }
        assert!(
            decode_remote_file_chunk(
                RemoteFileChunk {
                    bytes: b"short".to_vec(),
                    offset: 1,
                    total_bytes: 5,
                },
                RemoteFileChunkPosition::Tail,
            )
            .is_err()
        );
    }

    #[test]
    fn line_limit_keeps_the_requested_side_without_gaps() {
        let text = "line\n".repeat(MAX_REMOTE_FILE_PREVIEW_LINES + 1);
        let total = text.len() as u64;
        let first = decode_remote_file_chunk(
            RemoteFileChunk {
                bytes: text.as_bytes().to_vec(),
                offset: 0,
                total_bytes: total,
            },
            RemoteFileChunkPosition::From(0),
        )
        .expect("first line window should decode");
        assert_eq!(text_line_count(&first.text), MAX_REMOTE_FILE_PREVIEW_LINES);
        assert_eq!(first.offset, 0);
        assert!(first.end_offset < total);

        let tail = decode_remote_file_chunk(
            RemoteFileChunk {
                bytes: text.into_bytes(),
                offset: 0,
                total_bytes: total,
            },
            RemoteFileChunkPosition::Tail,
        )
        .expect("tail line window should decode");
        assert_eq!(text_line_count(&tail.text), MAX_REMOTE_FILE_PREVIEW_LINES);
        assert_eq!(tail.offset, 5);
        assert_eq!(tail.end_offset, total);
    }

    #[test]
    fn incremental_tail_merge_keeps_latest_bounded_text() {
        let current_text = "旧\n".repeat(MAX_REMOTE_FILE_PREVIEW_LINES);
        let current = RemoteFileText {
            total_bytes: current_text.len() as u64,
            offset: 0,
            end_offset: current_text.len() as u64,
            text: current_text,
        };
        let append_offset = current.end_offset;
        let appended = RemoteFileText {
            text: "新\n".into(),
            total_bytes: append_offset + 4,
            offset: append_offset,
            end_offset: append_offset + 4,
        };

        let merged = merge_remote_file_tail(&current, appended).expect("tail should merge");

        assert_eq!(text_line_count(&merged.text), MAX_REMOTE_FILE_PREVIEW_LINES);
        assert!(merged.text.ends_with("新\n"));
        assert_eq!(merged.end_offset, merged.total_bytes);
        assert_eq!(merged.offset, 4);
    }

    #[test]
    fn incremental_tail_merge_rejects_non_contiguous_data() {
        let current = RemoteFileText {
            text: "a".into(),
            total_bytes: 1,
            offset: 0,
            end_offset: 1,
        };
        let appended = RemoteFileText {
            text: "b".into(),
            total_bytes: 3,
            offset: 2,
            end_offset: 3,
        };

        assert!(merge_remote_file_tail(&current, appended).is_err());
    }

    #[test]
    fn incremental_tail_merge_trims_on_utf8_boundary() {
        let mut current_text = "é".to_string();
        current_text.push_str(&"a".repeat(MAX_REMOTE_FILE_PREVIEW_BYTES - 2));
        let current = RemoteFileText {
            total_bytes: current_text.len() as u64,
            offset: 0,
            end_offset: current_text.len() as u64,
            text: current_text,
        };
        let appended = RemoteFileText {
            text: "b".into(),
            total_bytes: current.end_offset + 1,
            offset: current.end_offset,
            end_offset: current.end_offset + 1,
        };

        let merged = merge_remote_file_tail(&current, appended).expect("tail should stay UTF-8");

        assert!(merged.text.len() <= MAX_REMOTE_FILE_PREVIEW_BYTES);
        assert_eq!(merged.offset, 2);
        assert!(merged.text.ends_with('b'));
    }
}
