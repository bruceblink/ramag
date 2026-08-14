use serde_json::json;

use super::delete::{delete_id_batches, mongo_response_u64};

#[test]
fn delete_batches_bound_count_and_estimated_bytes() {
    let by_count = delete_id_batches(vec![json!(1), json!(2), json!(3)], 2, usize::MAX).unwrap();
    assert_eq!(by_count.iter().map(Vec::len).collect::<Vec<_>>(), [2, 1]);

    let by_bytes = delete_id_batches(vec![json!("a"), json!("b"), json!("c")], 10, 130).unwrap();
    assert_eq!(by_bytes.iter().map(Vec::len).collect::<Vec<_>>(), [2, 1]);
}

#[test]
fn delete_batches_reject_a_single_oversized_id() {
    let result = delete_id_batches(vec![json!("oversized")], 10, 4);
    assert!(result.is_err());
}

#[test]
fn mongo_delete_count_accepts_int32_and_number_long() {
    assert_eq!(mongo_response_u64(Some(&json!(42))), Some(42));
    assert_eq!(
        mongo_response_u64(Some(&json!({"$numberLong": "5000000000"}))),
        Some(5_000_000_000)
    );
}
