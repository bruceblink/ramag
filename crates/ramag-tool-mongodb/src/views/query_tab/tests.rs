use super::find_command_template;

#[test]
fn collection_template_escapes_json_string_characters() {
    let template = find_command_template("quotes\"and\\slashes");
    let parsed: serde_json::Value = serde_json::from_str(&template).unwrap();
    assert_eq!(parsed["find"], "quotes\"and\\slashes");
    assert_eq!(parsed["sort"]["_id"], 1);
    assert!(parsed.get("limit").is_none());
}
