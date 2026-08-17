//! BSON ↔ serde_json::Value 双向转换。BSON 特殊类型走 Extended JSON 风格：
//! MongoDB 扩展 JSON 类型转换。

use bson::{Bson, Document};
use ramag_domain::error::{DomainError, Result};
use serde_json::Value;

/// BSON Bson → serde_json::Value。整体走 relaxed Extended JSON，**但 Int64 显式包成
/// `{"$numberLong":"N"}`**（其余类型仍 relaxed，可读性不变）。
/// 为何特化 Int64：relaxed 下 Int32/Int64 都输出裸数字、无法区分，导致单元格编辑一个
/// Int64 正小值（≤ i32::MAX）时经 serde 反序列化被窄化成 Int32（静默改 BSON 类型）。
/// 包装后 cell 层可标 "long" kind、编辑时按 $numberLong 还原写回，保住 64 位类型。
pub fn bson_to_json(b: Bson) -> Value {
    bson_to_json_int64_safe(b)
}

pub fn document_to_json(doc: Document) -> Value {
    bson_to_json_int64_safe(Bson::Document(doc))
}

/// 递归转换：仅 Int64 特化为 `$numberLong` 包装；容器（Document/Array）递归以覆盖嵌套 Int64；
/// 其余叶子类型逐个委托 `into_relaxed_extjson`，与原行为完全一致
fn bson_to_json_int64_safe(b: Bson) -> Value {
    match b {
        Bson::Int64(n) => serde_json::json!({ "$numberLong": n.to_string() }),
        Bson::Document(doc) => Value::Object(
            doc.into_iter()
                .map(|(k, v)| (k, bson_to_json_int64_safe(v)))
                .collect(),
        ),
        Bson::Array(arr) => Value::Array(arr.into_iter().map(bson_to_json_int64_safe).collect()),
        other => other.into_relaxed_extjson(),
    }
}

/// serde_json::Value → BSON Bson。识别 Extended JSON 形态（$oid / $numberDecimal 等）。
/// 借 bson 的 serde::Deserialize impl，统一走 serde 反序列化
pub fn json_to_bson(v: Value) -> Result<Bson> {
    serde_json::from_value(v)
        .map_err(|e| DomainError::InvalidConfig(format!("JSON 解析 BSON 失败：{e}")))
}

/// 强制返回 Document（顶层必须是对象）。filter / update / sort / projection 等场景用
pub fn json_to_document(v: Value) -> Result<Document> {
    match json_to_bson(v)? {
        Bson::Document(d) => Ok(d),
        other => Err(DomainError::InvalidConfig(format!(
            "期望 JSON 对象，实际：{other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn int64_wrapped_as_numberlong() {
        // Int64 必须包成 $numberLong（含嵌套 doc / array），否则编辑正小值会被窄化成 Int32
        let doc = bson::doc! { "big": 42_i64, "nested": { "n": 7_i64 }, "arr": [9_i64] };
        let v = document_to_json(doc);
        assert_eq!(v["big"], json!({ "$numberLong": "42" }));
        assert_eq!(v["nested"]["n"], json!({ "$numberLong": "7" }));
        assert_eq!(v["arr"][0], json!({ "$numberLong": "9" }));
    }

    #[test]
    fn int32_stays_bare_number() {
        // Int32 仍是裸数字（relaxed），与 Int64 的 $numberLong 包装区分开
        let v = document_to_json(bson::doc! { "small": 42_i32 });
        assert_eq!(v["small"], json!(42));
    }

    #[test]
    fn numberlong_roundtrips_to_int64() {
        // 编辑写回：$numberLong 包装经 json_to_bson 还原为真正的 Int64（不窄化）
        assert_eq!(
            json_to_bson(json!({ "$numberLong": "42" })).unwrap(),
            Bson::Int64(42)
        );
    }

    #[test]
    fn roundtrip_basic_doc() {
        let v = json!({"a": 1, "b": "hello", "c": [1, 2, 3]});
        let doc = json_to_document(v.clone()).unwrap();
        let back = document_to_json(doc);
        assert_eq!(back["a"], json!(1));
        assert_eq!(back["b"], json!("hello"));
        assert_eq!(back["c"], json!([1, 2, 3]));
    }

    #[test]
    fn objectid_extjson_roundtrip() {
        let v = json!({"$oid": "507f1f77bcf86cd799439011"});
        let bson = json_to_bson(v.clone()).unwrap();
        assert!(matches!(bson, Bson::ObjectId(_)));
        let back = bson_to_json(bson);
        // 走 extjson 后会保留 $oid 包装
        assert_eq!(back["$oid"], json!("507f1f77bcf86cd799439011"));
    }

    #[test]
    fn non_object_top_level_rejected() {
        let v = json!([1, 2, 3]);
        assert!(json_to_document(v).is_err());
    }

    #[test]
    fn update_set_doc_preserved() {
        // update 文档 {$set: {...}} 转 BSON 后 $set 必须保留为子文档（否则 update_one 无效）
        let v = json!({"$set": {"name": "Bob", "age": 30}});
        let doc = json_to_document(v).unwrap();
        let set = doc.get_document("$set").expect("$set 应保留为子文档");
        assert_eq!(set.get_str("name").unwrap(), "Bob");
    }

    #[test]
    fn filter_id_oid_becomes_objectid() {
        // filter {_id: {$oid}} 必须转成真正的 ObjectId，否则匹配不到文档
        let v = json!({"_id": {"$oid": "507f1f77bcf86cd799439011"}});
        let doc = json_to_document(v).unwrap();
        assert!(matches!(doc.get("_id"), Some(Bson::ObjectId(_))));
    }
}
