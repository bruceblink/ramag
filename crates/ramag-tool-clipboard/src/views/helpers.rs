//! 纯函数：列表过滤 + 相对时间格式化（便于测试，不依赖 GPUI）

use chrono::{DateTime, Utc};
use ramag_domain::entities::{ClipItem, ClipKind, contains_case_insensitive};

/// 即时层每条正文最多扫描此前缀；完整正文由去抖后的后台存储搜索覆盖。
const MAX_IMMEDIATE_TEXT_SEARCH_BYTES: usize = 4 * 1024;

/// 过滤 + 排序：按 last_used_at desc。
/// 搜索即时匹配 preview / 正文有界前缀（大小写不敏感）；kind=None 不限类型。
/// 更深正文由后台全量搜索补齐，避免每次按键在 UI 线程扫描最多 64 MiB 缓存。
pub fn filter_items<'a, T>(items: &'a [T], query: &str, kind: Option<ClipKind>) -> Vec<&'a T>
where
    T: std::borrow::Borrow<ClipItem>,
{
    let q = query.trim().to_lowercase();
    let mut out: Vec<&T> = items
        .iter()
        .filter(|item| {
            let clip = <T as std::borrow::Borrow<ClipItem>>::borrow(*item);
            kind.is_none_or(|expected| clip.kind == expected)
        })
        .filter(|item| {
            let clip = <T as std::borrow::Borrow<ClipItem>>::borrow(*item);
            q.is_empty() || matches_query(clip, &q)
        })
        .collect();
    // service 缓存与存储搜索结果都已是最近优先；普通渲染无需再做 O(n log n) 排序。
    // 保留乱序输入兼容，仅在确实发现逆序对时排序。
    let out_of_order = out.windows(2).any(|pair| {
        let left = <T as std::borrow::Borrow<ClipItem>>::borrow(pair[0]);
        let right = <T as std::borrow::Borrow<ClipItem>>::borrow(pair[1]);
        left.last_used_at < right.last_used_at
    });
    if out_of_order {
        out.sort_by_key(|item| {
            std::cmp::Reverse(<T as std::borrow::Borrow<ClipItem>>::borrow(*item).last_used_at)
        });
    }
    out
}

fn matches_query(item: &ClipItem, q_lower: &str) -> bool {
    if contains_case_insensitive(&item.preview, q_lower) {
        return true;
    }
    item.text
        .as_deref()
        .is_some_and(|text| contains_case_insensitive(text_prefix(text), q_lower))
}

fn text_prefix(text: &str) -> &str {
    let mut end = text.len().min(MAX_IMMEDIATE_TEXT_SEARCH_BYTES);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// 相对时间：刚刚 / N 分钟前 / N 小时前 / N 天前 / 日期
pub fn relative_time(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - then).num_seconds().max(0);
    match secs {
        0..=59 => "刚刚".to_string(),
        60..=3599 => format!("{} 分钟前", secs / 60),
        3600..=86399 => format!("{} 小时前", secs / 3600),
        86400..=604_799 => format!("{} 天前", secs / 86400),
        _ => then.format("%Y-%m-%d").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use chrono::Duration;
    use ramag_domain::entities::{ClipId, fnv1a_hash};

    fn clip(text: &str, kind: ClipKind, age_secs: i64) -> ClipItem {
        let at = Utc::now() - Duration::seconds(age_secs);
        ClipItem {
            id: ClipId::new(),
            kind,
            text: Some(text.to_string()),
            rtf: None,
            image_path: None,
            thumb_path: None,
            image_dims: None,
            files: Vec::new(),
            preview: text.to_string(),
            source: None,
            byte_size: 0,
            content_hash: format!("{:016x}", fnv1a_hash(text.as_bytes())),
            created_at: at,
            last_used_at: at,
        }
    }

    #[test]
    fn sorted_by_recency() {
        let items = vec![
            clip("old", ClipKind::Text, 100),
            clip("new", ClipKind::Text, 1),
            clip("mid", ClipKind::Text, 50),
        ];
        let out = filter_items(&items, "", None);
        assert_eq!(out[0].text.as_deref(), Some("new"));
        assert_eq!(out[1].text.as_deref(), Some("mid"));
        assert_eq!(out[2].text.as_deref(), Some("old"));
    }

    #[test]
    fn filter_by_kind_and_query() {
        let items = vec![
            clip("hello world", ClipKind::Text, 1),
            clip("https://x.com", ClipKind::Link, 1),
        ];
        assert_eq!(filter_items(&items, "", Some(ClipKind::Link)).len(), 1);
        assert_eq!(filter_items(&items, "HELLO", None).len(), 1);
        assert_eq!(filter_items(&items, "zzz", None).len(), 0);
    }

    #[test]
    fn filter_keeps_shared_clip_payloads() {
        let item = Arc::new(clip("large shared text", ClipKind::Text, 1));
        let items = vec![item.clone()];
        let filtered = filter_items(&items, "shared", None);
        assert!(Arc::ptr_eq(filtered[0], &item));
    }

    #[test]
    fn immediate_filter_bounds_large_text_scan() {
        let text = format!("{}needle", "a".repeat(MAX_IMMEDIATE_TEXT_SEARCH_BYTES));
        let mut item = clip(&text, ClipKind::Text, 1);
        item.preview = "a".into();

        assert!(filter_items(&[item], "needle", None).is_empty());
    }

    #[test]
    fn relative_time_buckets() {
        let now = Utc::now();
        assert_eq!(relative_time(now, now), "刚刚");
        assert_eq!(relative_time(now - Duration::minutes(5), now), "5 分钟前");
        assert_eq!(relative_time(now - Duration::hours(3), now), "3 小时前");
        assert_eq!(relative_time(now - Duration::days(2), now), "2 天前");
    }
}
