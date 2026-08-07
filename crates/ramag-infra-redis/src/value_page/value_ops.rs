use super::*;

/// HSCAN 应答严格解码：二进制 field 无法用实体表达，跳过并计数（不做 lossy 转换）
pub(super) fn strict_hash_pairs(v: RV) -> Result<(Vec<(String, RedisValue)>, u64)> {
    let mut pairs = Vec::new();
    let mut skipped = 0u64;
    match v {
        RV::Nil => {}
        RV::Map(entries) => {
            for (k, value) in entries {
                match field_name(k) {
                    Some(field) => pairs.push((field, decode_value(value))),
                    None => skipped += 1,
                }
            }
        }
        RV::Array(flat) => {
            if flat.len() % 2 != 0 {
                return Err(DomainError::QueryFailed(format!(
                    "HSCAN 应答长度非偶数：{}",
                    flat.len()
                )));
            }
            let mut iter = flat.into_iter();
            while let (Some(k), Some(value)) = (iter.next(), iter.next()) {
                match field_name(k) {
                    Some(field) => pairs.push((field, decode_value(value))),
                    None => skipped += 1,
                }
            }
        }
        other => {
            return Err(DomainError::QueryFailed(format!(
                "HSCAN 应答格式异常：{other:?}"
            )));
        }
    }
    Ok((pairs, skipped))
}

/// XRANGE 应答严格解码。返回（成功条目, 原始条目数, 最后一条原始 id, 跳过数）；
/// 分页游标必须基于原始条目数与原始最后 id，跳过的条目同样推进游标
pub(super) fn strict_stream_entries(
    v: RV,
) -> Result<(Vec<StreamEntry>, usize, Option<String>, u64)> {
    let raw = match v {
        RV::Array(a) => a,
        RV::Nil => Vec::new(),
        other => {
            return Err(DomainError::QueryFailed(format!(
                "XRANGE 应答非数组：{other:?}"
            )));
        }
    };
    let raw_count = raw.len();
    let mut out = Vec::with_capacity(raw_count);
    let mut last_id = None;
    let mut skipped = 0u64;
    for entry in raw {
        let RV::Array(mut parts) = entry else {
            return Err(DomainError::QueryFailed("Stream entry 非数组".into()));
        };
        if parts.len() != 2 {
            return Err(DomainError::QueryFailed(format!(
                "Stream entry 期望 2 元素，实得 {}",
                parts.len()
            )));
        }
        let fields_raw = parts.pop().unwrap_or(RV::Nil);
        let id_raw = parts.pop().unwrap_or(RV::Nil);
        let Some(id) = field_name(id_raw) else {
            return Err(DomainError::QueryFailed("Stream entry id 非 UTF-8".into()));
        };
        last_id = Some(id.clone());
        match strict_stream_fields(fields_raw) {
            Some(fields) => out.push(StreamEntry { id, fields }),
            None => skipped += 1,
        }
    }
    Ok((out, raw_count, last_id, skipped))
}

/// field 与 value 任一非 UTF-8 即整条跳过（实体是 (String, String)）
pub(super) fn strict_stream_fields(v: RV) -> Option<Vec<(String, String)>> {
    let RV::Array(flat) = v else { return None };
    if flat.len() % 2 != 0 {
        return None;
    }
    let mut pairs = Vec::with_capacity(flat.len() / 2);
    let mut iter = flat.into_iter();
    while let (Some(k), Some(value)) = (iter.next(), iter.next()) {
        pairs.push((field_name(k)?, field_name(value)?));
    }
    Some(pairs)
}

pub(super) fn field_name(v: RV) -> Option<String> {
    match v {
        RV::SimpleString(s) => Some(s),
        RV::BulkString(bytes) => String::from_utf8(bytes).ok(),
        _ => None,
    }
}

pub(super) async fn append_chunk(
    mgr: &mut ConnectionManager,
    key: &str,
    chunk: &[u8],
) -> Result<u64> {
    ensure_member_size(chunk.len())?;
    // 空串单独走 SET：确保空 string 值也能建 key
    if chunk.is_empty() {
        let _: RV = redis::cmd("SET")
            .arg(key)
            .arg(chunk)
            .query_async(mgr)
            .await
            .map_err(map_redis_error)?;
        return Ok(0);
    }
    let _: RV = redis::cmd("APPEND")
        .arg(key)
        .arg(chunk)
        .query_async(mgr)
        .await
        .map_err(map_redis_error)?;
    Ok(1)
}

pub(super) async fn write_members(
    mgr: &mut ConnectionManager,
    key: &str,
    command: &str,
    members: &[RedisValue],
) -> Result<u64> {
    let mut written = 0u64;
    let mut cmd = new_key_cmd(command, key);
    let mut pending = 0usize;
    let mut pending_bytes = 0usize;
    for member in members {
        let arg = member_arg(member)?;
        ensure_member_size(arg.len())?;
        if pending > 0
            && (pending >= WRITE_CHUNK_MEMBERS
                || pending_bytes.saturating_add(arg.len()) > WRITE_CHUNK_BYTES)
        {
            flush(mgr, cmd).await?;
            cmd = new_key_cmd(command, key);
            pending = 0;
            pending_bytes = 0;
        }
        cmd.arg(arg.as_ref());
        pending += 1;
        pending_bytes = pending_bytes.saturating_add(arg.len());
        written += 1;
    }
    if pending > 0 {
        flush(mgr, cmd).await?;
    }
    Ok(written)
}

pub(super) async fn write_hash(
    mgr: &mut ConnectionManager,
    key: &str,
    pairs: &[(String, RedisValue)],
) -> Result<u64> {
    let mut written = 0u64;
    let mut cmd = new_key_cmd("HSET", key);
    let mut pending = 0usize;
    let mut pending_bytes = 0usize;
    for (field, value) in pairs {
        let arg = member_arg(value)?;
        ensure_member_size(field.len())?;
        ensure_member_size(arg.len())?;
        let pair_bytes = field.len().saturating_add(arg.len());
        if pending > 0
            && (pending >= WRITE_CHUNK_MEMBERS
                || pending_bytes.saturating_add(pair_bytes) > WRITE_CHUNK_BYTES)
        {
            flush(mgr, cmd).await?;
            cmd = new_key_cmd("HSET", key);
            pending = 0;
            pending_bytes = 0;
        }
        cmd.arg(field).arg(arg.as_ref());
        pending += 1;
        pending_bytes = pending_bytes.saturating_add(pair_bytes);
        written += 1;
    }
    if pending > 0 {
        flush(mgr, cmd).await?;
    }
    Ok(written)
}

pub(super) async fn write_zset(
    mgr: &mut ConnectionManager,
    key: &str,
    pairs: &[(RedisValue, f64)],
) -> Result<u64> {
    let mut written = 0u64;
    let mut cmd = new_key_cmd("ZADD", key);
    let mut pending = 0usize;
    let mut pending_bytes = 0usize;
    for (member, score) in pairs {
        let arg = member_arg(member)?;
        ensure_member_size(arg.len())?;
        let score_text = format_score(*score)?;
        let pair_bytes = arg.len().saturating_add(score_text.len());
        if pending > 0
            && (pending >= WRITE_CHUNK_MEMBERS
                || pending_bytes.saturating_add(pair_bytes) > WRITE_CHUNK_BYTES)
        {
            flush(mgr, cmd).await?;
            cmd = new_key_cmd("ZADD", key);
            pending = 0;
            pending_bytes = 0;
        }
        cmd.arg(score_text).arg(arg.as_ref());
        pending += 1;
        pending_bytes = pending_bytes.saturating_add(pair_bytes);
        written += 1;
    }
    if pending > 0 {
        flush(mgr, cmd).await?;
    }
    Ok(written)
}

pub(super) async fn write_stream(
    mgr: &mut ConnectionManager,
    key: &str,
    entries: &[StreamEntry],
) -> Result<u64> {
    let mut written = 0u64;
    for entry in entries {
        if entry.fields.is_empty() {
            return Err(DomainError::InvalidConfig(format!(
                "Stream entry {} 缺少字段，无法 XADD",
                entry.id
            )));
        }
        validate_stream_entry_budget(key, entry)?;
        let mut cmd = redis::cmd("XADD");
        cmd.arg(key).arg(&entry.id);
        for (field, value) in &entry.fields {
            ensure_member_size(field.len())?;
            ensure_member_size(value.len())?;
            cmd.arg(field).arg(value);
        }
        flush(mgr, cmd).await?;
        written += 1;
    }
    Ok(written)
}

pub(super) fn validate_stream_entry_budget(key: &str, entry: &StreamEntry) -> Result<()> {
    ensure_member_size(entry.id.len())?;
    let mut bytes = key
        .len()
        .checked_add(entry.id.len())
        .ok_or_else(|| DomainError::InvalidConfig("Redis Stream 条目长度溢出".into()))?;
    for (field, value) in &entry.fields {
        ensure_member_size(field.len())?;
        ensure_member_size(value.len())?;
        bytes = bytes
            .checked_add(field.len())
            .and_then(|total| total.checked_add(value.len()))
            .ok_or_else(|| DomainError::InvalidConfig("Redis Stream 条目长度溢出".into()))?;
    }
    if bytes > WRITE_CHUNK_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "Redis Stream 单条记录超过 {} MiB 写入上限",
            WRITE_CHUNK_BYTES / 1024 / 1024
        )));
    }
    Ok(())
}

pub(super) fn new_key_cmd(command: &str, key: &str) -> redis::Cmd {
    let mut cmd = redis::cmd(command);
    cmd.arg(key);
    cmd
}

pub(super) async fn flush(mgr: &mut ConnectionManager, cmd: redis::Cmd) -> Result<()> {
    let _: RV = cmd.query_async(mgr).await.map_err(map_redis_error)?;
    Ok(())
}

pub(super) fn ensure_member_size(bytes: usize) -> Result<()> {
    if bytes > MAX_REDIS_COMMAND_ARG_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "Redis 成员超过 {} MiB 上限，无法写入",
            MAX_REDIS_COMMAND_ARG_BYTES / 1024 / 1024
        )));
    }
    Ok(())
}

/// 集合成员 → 命令参数字节。Text/Bytes 原样；Int/Float 十进制文本（读取端会把数字
/// 形态的应答解码成 Int/Float，写回时还原成原始文本形态）
pub(super) fn member_arg(value: &RedisValue) -> Result<Cow<'_, [u8]>> {
    match value {
        RedisValue::Text(text) => Ok(Cow::Borrowed(text.as_bytes())),
        RedisValue::Bytes(bytes) => Ok(Cow::Borrowed(bytes.as_slice())),
        RedisValue::Int(number) => Ok(Cow::Owned(number.to_string().into_bytes())),
        RedisValue::Float(number) => Ok(Cow::Owned(number.to_string().into_bytes())),
        other => Err(DomainError::InvalidConfig(format!(
            "该成员类型不支持导入写入：{}",
            other.display_preview(32)
        ))),
    }
}

pub(super) fn format_score(score: f64) -> Result<String> {
    if score.is_nan() {
        return Err(DomainError::InvalidConfig("ZSet score 不能是 NaN".into()));
    }
    if score == f64::INFINITY {
        return Ok("+inf".into());
    }
    if score == f64::NEG_INFINITY {
        return Ok("-inf".into());
    }
    Ok(score.to_string())
}
