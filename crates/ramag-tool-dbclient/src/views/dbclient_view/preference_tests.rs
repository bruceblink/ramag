use super::*;

#[test]
fn open_sessions_parser_accepts_new_and_legacy_formats() {
    let id = ramag_domain::entities::ConnectionId::new();
    let modern = serde_json::to_string(&OpenSessionsPref {
        ids: vec![id.clone()],
        active: Some(id.clone()),
    })
    .unwrap_or_default();
    let legacy = serde_json::to_string(&vec![id.clone()]).unwrap_or_default();

    assert!(matches!(
        parse_open_sessions(&modern),
        Ok((pref, false)) if pref.ids == vec![id.clone()] && pref.active == Some(id.clone())
    ));
    assert!(matches!(
        parse_open_sessions(&legacy),
        Ok((pref, false)) if pref.ids == vec![id] && pref.active.is_none()
    ));
    assert!(parse_open_sessions("not-json").is_err());
}

#[test]
fn open_sessions_parser_bounds_and_deduplicates_restore_data() {
    let first = ramag_domain::entities::ConnectionId::new();
    let mut ids = vec![first.clone(), first.clone()];
    ids.extend((0..MAX_CONNECTION_SESSIONS).map(|_| ramag_domain::entities::ConnectionId::new()));
    let json = serde_json::to_string(&OpenSessionsPref {
        ids,
        active: Some(first.clone()),
    })
    .unwrap_or_default();

    assert!(matches!(
        parse_open_sessions(&json),
        Ok((pref, true))
            if pref.ids.len() == MAX_CONNECTION_SESSIONS
                && pref.ids.first() == Some(&first)
                && pref.active == Some(first)
    ));
    assert!(parse_open_sessions(&" ".repeat(MAX_OPEN_SESSIONS_PREF_BYTES + 1)).is_err());
}
