use super::parse::{
    MAX_COMMIT_PARENTS, MAX_COMMIT_REFS, parse_log_list_output, parse_log_output, parse_record,
    parse_refs,
};
use super::*;

#[test]
fn parses_two_records() -> Result<()> {
    let raw = "abc123\x1fAlice\x1falice@x.com\x1f1700000000\x1fAlice\x1falice@x.com\x1f1700000000\x1f\x1fHEAD -> main, tag: v1.0\x1ffirst commit\x1f\x1edef456\x1fBob\x1fbob@x.com\x1f1700001000\x1fBob\x1fbob@x.com\x1f1700001000\x1fabc123\x1f\x1ffix bug\x1ffull body\x1e";
    let commits = parse_log_output(raw)?;
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].id.0, "abc123");
    assert_eq!(commits[0].subject, "first commit");
    assert_eq!(commits[0].author.name, "Alice");
    assert_eq!(commits[0].refs, vec!["HEAD -> main", "tag: v1.0"]);
    assert_eq!(commits[1].parents.len(), 1);
    assert_eq!(commits[1].parents[0].0, "abc123");
    assert_eq!(commits[1].body, "full body");
    assert!(commits[1].refs.is_empty());
    Ok(())
}

#[test]
fn parses_lightweight_list_records() -> Result<()> {
    let raw = "abc123\x1fAlice\x1f1700000000\x1fparent\x1fHEAD -> main\x1fsubject\x1e";
    let commits = parse_log_list_output(raw)?;

    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].author.name, "Alice");
    assert!(commits[0].author.email.is_empty());
    assert!(commits[0].committer.name.is_empty());
    assert_eq!(commits[0].parents[0].0, "parent");
    assert_eq!(commits[0].subject, "subject");
    assert!(commits[0].body.is_empty());
    Ok(())
}

#[test]
fn streaming_reader_returns_exact_records() -> Result<()> {
    let input = b"first\x1esecond\x1e";
    let mut reader = std::io::BufReader::with_capacity(3, std::io::Cursor::new(input));

    assert_eq!(read_log_record(&mut reader)?, Some(b"first".to_vec()));
    assert_eq!(read_log_record(&mut reader)?, Some(b"second".to_vec()));
    assert_eq!(read_log_record(&mut reader)?, None);
    Ok(())
}

#[test]
fn empty_input() -> Result<()> {
    assert_eq!(parse_log_output("")?.len(), 0);
    Ok(())
}

#[test]
fn malformed_record_is_reported() {
    let raw = "abc123\x1fAlice\x1falice@x.com\x1fnot-a-time\x1e";
    assert!(parse_log_output(raw).is_err());
}

#[test]
fn pathological_parent_count_is_rejected() {
    let parents = (0..=MAX_COMMIT_PARENTS)
        .map(|index| format!("{index:040x}"))
        .collect::<Vec<_>>()
        .join(" ");
    let raw = format!(
        "abc123\x1fA\x1fa@x\x1f1700000000\x1fA\x1fa@x\x1f1700000000\x1f{parents}\x1f\x1fsubject\x1f"
    );

    assert!(parse_record(&raw).is_err());
}

#[test]
fn excessive_refs_are_truncated_with_a_hint() {
    let raw = (0..(MAX_COMMIT_REFS + 3))
        .map(|index| format!("ref-{index}"))
        .collect::<Vec<_>>()
        .join(",");

    let refs = parse_refs(&raw);

    assert_eq!(refs.len(), MAX_COMMIT_REFS);
    assert!(refs.last().is_some_and(|value| value.contains("省略")));
}

#[test]
fn non_repository_error_is_not_treated_as_empty_history()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    assert!(run_log(temp.path(), &LogOptions::default()).is_err());
    Ok(())
}

#[test]
fn log_args_keep_the_selected_ref_as_the_history_start() {
    for start in [
        "refs/heads/feature/ui",
        "refs/remotes/origin/main",
        "refs/tags/v1.2.3",
    ] {
        let options = LogOptions {
            start: Some(start.into()),
            limit: Some(100),
            ..Default::default()
        };

        let args = build_log_args(&options, true);

        assert!(
            args.iter().any(|argument| argument == start),
            "selected history ref was not passed to git: {args:?}"
        );
    }
}
