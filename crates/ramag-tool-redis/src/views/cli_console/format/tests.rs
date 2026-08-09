use super::*;

#[test]
fn tokenize_plain() {
    assert_eq!(tokenize("GET foo").unwrap(), vec!["GET", "foo"]);
    assert_eq!(tokenize("  PING  ").unwrap(), vec!["PING"]);
    assert!(tokenize("   ").unwrap().is_empty());
}

#[test]
fn tokenize_quoted() {
    assert_eq!(
        tokenize(r#"SET k "a b c""#).unwrap(),
        vec!["SET", "k", "a b c"]
    );
    assert_eq!(tokenize(r#"SET k 'a b'"#).unwrap(), vec!["SET", "k", "a b"]);
    // 双引号内转义
    assert_eq!(
        tokenize(r#"SET k "a\tb""#).unwrap(),
        vec!["SET", "k", "a\tb"]
    );
    // 裸段与引号段拼接
    assert_eq!(tokenize(r#"foo"bar""#).unwrap(), vec!["foobar"]);
}

#[test]
fn tokenize_unbalanced() {
    assert!(tokenize(r#"SET k "unclosed"#).is_err());
    assert!(tokenize("SET k 'unclosed").is_err());
}

#[test]
fn tokenize_hex_escape_without_temporary_string() {
    assert_eq!(
        tokenize(r#"SET key "\x41\x7a\x2F""#).unwrap(),
        vec!["SET", "key", "Az/"]
    );
    assert!(tokenize(r#"SET key "\xG0""#).is_err());
    assert!(tokenize(r#"SET key "\xFF""#).is_err());
}

#[test]
fn format_scalars() {
    assert_eq!(lines_of(&RedisValue::Nil), vec!["(nil)"]);
    assert_eq!(lines_of(&RedisValue::Int(42)), vec!["(integer) 42"]);
    assert_eq!(lines_of(&RedisValue::Text("bar".into())), vec!["\"bar\""]);
}

#[test]
fn format_text_json_pretty() {
    // JSON String 值应多行美化（非单行加引号）
    let lines = lines_of(&RedisValue::Text(r#"{"a":1,"b":2}"#.into()));
    assert!(lines.len() > 1, "JSON 应多行: {lines:?}");
    assert!(lines.iter().any(|l| l.contains("\"a\"")));
}

#[test]
fn format_bytes_hex() {
    let v = RedisValue::Bytes(vec![0xac, 0x41, 0x00]);
    assert_eq!(lines_of(&v), vec!["\"\\xacA\\x00\""]);
}

#[test]
fn format_nested_array() {
    let v = RedisValue::Array(vec![
        RedisValue::Text("a".into()),
        RedisValue::Array(vec![RedisValue::Int(1), RedisValue::Int(2)]),
    ]);
    let lines = lines_of(&v);
    assert_eq!(
        lines,
        vec!["1) \"a\"", "2) 1) (integer) 1", "   2) (integer) 2"]
    );
}

#[test]
fn format_hash_inline() {
    let v = RedisValue::Hash(vec![("f".into(), RedisValue::Text("v".into()))]);
    assert_eq!(lines_of(&v), vec!["1) \"f\" => \"v\""]);
}

#[test]
fn format_empty() {
    assert_eq!(lines_of(&RedisValue::Array(vec![])), vec!["(empty)"]);
}

#[test]
fn chunked_scalar_reassembles_full_value_via_cursor() {
    // 超限文本分段：沿游标取完所有段，拼回完整值（首尾带引号）
    let value = "y".repeat(MAX_SCALAR_INPUT_BYTES + MAX_SCALAR_INPUT_BYTES / 2);
    let v = RedisValue::Text(value.clone());
    let mut all = String::new();
    let mut chunk = lines_of_first(&v);
    let mut rounds = 0;
    loop {
        for line in &chunk.lines {
            all.push_str(line);
        }
        rounds += 1;
        match chunk.cursor {
            Some(cursor) => chunk = lines_of_more(&v, cursor),
            None => break,
        }
    }
    assert!(rounds > 1, "超限值应分成多段");
    assert_eq!(all, format!("\"{value}\""));
}

#[test]
fn chunked_container_resumes_at_element_boundary_with_global_numbering() {
    // 容器分段：单元素放不下时停在元素边界，续段编号全局连续
    let big = "z".repeat(MAX_FORMAT_BYTES / 2);
    let items = vec![
        RedisValue::Text(big.clone()),
        RedisValue::Text(big),
        RedisValue::Int(7),
    ];
    let v = RedisValue::Array(items);
    let first = lines_of_first(&v);
    let cursor = first.cursor.expect("应有续展开游标");
    let more = lines_of_more(&v, cursor);
    let joined = more.lines.join("\n");
    assert!(joined.contains("3) (integer) 7"), "续段应保持全局编号");
}

#[test]
fn max_scalar_keeps_content_instead_of_marker_only() {
    // 满上限的 8 MiB 标量必须保留内容行；此前预算 off-by-one 会把内容行弹光只剩截断标记
    let s = "x".repeat(MAX_SCALAR_INPUT_BYTES);
    let lines = lines_of(&RedisValue::Text(s));
    assert!(lines.iter().any(|line| line.len() > 1_000));
}

#[test]
fn formatting_is_bounded_by_size_depth_and_lines() {
    let huge = RedisValue::Bytes(vec![0xff; MAX_SCALAR_INPUT_BYTES + 1]);
    let output = lines_of(&huge).join("\n");
    assert!(output.len() <= MAX_FORMAT_BYTES);
    assert!(output.contains(TRUNCATION_LINE));

    let many = RedisValue::Array(vec![RedisValue::Int(1); MAX_FORMAT_LINES + 1]);
    let lines = lines_of(&many);
    assert!(lines.len() <= MAX_FORMAT_LINES);
    assert_eq!(lines.last().map(String::as_str), Some(TRUNCATION_LINE));

    let mut deep = RedisValue::Nil;
    for _ in 0..=MAX_FORMAT_DEPTH {
        deep = RedisValue::Array(vec![deep]);
    }
    assert!(lines_of(&deep).join("\n").contains("嵌套层级过深"));
}
