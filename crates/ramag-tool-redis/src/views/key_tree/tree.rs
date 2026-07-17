//! 扁平 key 按 `:` 建 Trie 多层命名空间树

use std::collections::{BTreeMap, HashSet};

use ramag_domain::entities::{KeyMeta, RedisType};

use super::NAMESPACE_SEP;

/// UI 命名空间最大深度；更深的合法 key 将剩余后缀折叠到最后一层，完整 key 不变。
const MAX_NAMESPACE_DEPTH: usize = 16;

/// 树节点：可同时是命名空间（有子节点）和叶子（对应实际 key）
#[derive(Debug, Clone)]
pub(super) struct TreeNode {
    /// 当前层显示标签（路径中的一段）
    pub(super) label: String,
    /// 子节点（按 label 排序：命名空间在前，叶子在后；同类按字母升序）
    pub(super) children: Vec<TreeNode>,
    /// 该节点本身是否对应实际 key（SCAN 不查类型，bare key 的 leaf_type 为 None，
    /// 不能用 leaf_type.is_some() 判定叶子）
    pub(super) is_key: bool,
    /// key 的类型（None=未查询；仅用于类型徽标显示）
    pub(super) leaf_type: Option<RedisType>,
}

impl TreeNode {
    pub(super) fn is_namespace(&self) -> bool {
        !self.children.is_empty()
    }
}

/// 渲染层用的扁平行（拥有数据，避免与 cx.listener 借用冲突）
#[derive(Debug, Clone)]
pub(super) struct VisibleRow {
    pub(super) depth: usize,
    pub(super) label: String,
    pub(super) full_path: String,
    pub(super) is_key: bool,
    pub(super) leaf_type: Option<RedisType>,
    pub(super) is_namespace: bool,
    pub(super) is_expanded: bool,
}

#[derive(Default)]
struct NodeBuilder {
    children: BTreeMap<String, NodeBuilder>,
    is_key: bool,
    leaf_type: Option<RedisType>,
}

pub(super) fn build_tree(keys: &[KeyMeta]) -> Vec<TreeNode> {
    let mut roots: BTreeMap<String, NodeBuilder> = BTreeMap::new();
    for key in keys {
        if key.key.is_empty() || key.key.split(NAMESPACE_SEP).any(str::is_empty) {
            // 跳过空 key 或形如 "::" 的异常路径
            continue;
        }
        let mut siblings = &mut roots;
        let mut parts = key
            .key
            .splitn(MAX_NAMESPACE_DEPTH, NAMESPACE_SEP)
            .peekable();
        while let Some(part) = parts.next() {
            let is_last = parts.peek().is_none();
            let node = siblings.entry(part.to_string()).or_default();
            if is_last {
                node.is_key = true;
                node.leaf_type = key.key_type;
            }
            siblings = &mut node.children;
        }
    }
    finish_nodes(roots)
}

fn finish_nodes(builders: BTreeMap<String, NodeBuilder>) -> Vec<TreeNode> {
    let mut namespaces = Vec::new();
    let mut leaves = Vec::new();
    for (label, builder) in builders {
        let children = finish_nodes(builder.children);
        let node = TreeNode {
            label,
            is_key: builder.is_key,
            leaf_type: builder.leaf_type,
            children,
        };
        if node.is_namespace() {
            namespaces.push(node);
        } else {
            leaves.push(node);
        }
    }
    namespaces.extend(leaves);
    namespaces
}

pub(super) fn collect_namespace_paths(node: &TreeNode, out: &mut HashSet<String>) {
    collect_namespace_paths_from(node, "", out);
}

fn collect_namespace_paths_from(node: &TreeNode, parent: &str, out: &mut HashSet<String>) {
    let full_path = joined_path(parent, &node.label);
    if node.is_namespace() {
        out.insert(full_path.clone());
        for c in &node.children {
            collect_namespace_paths_from(c, &full_path, out);
        }
    }
}

fn joined_path(parent: &str, label: &str) -> String {
    if parent.is_empty() {
        return label.to_string();
    }
    let mut path = String::with_capacity(parent.len().saturating_add(label.len() + 1));
    path.push_str(parent);
    path.push(NAMESPACE_SEP);
    path.push_str(label);
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(key: &str, t: RedisType) -> KeyMeta {
        KeyMeta {
            key: key.to_string(),
            key_type: Some(t),
            ttl_ms: None,
        }
    }

    #[test]
    fn build_simple_tree() {
        let keys = vec![
            meta("user:1:profile", RedisType::Hash),
            meta("user:2:profile", RedisType::Hash),
            meta("session:abc", RedisType::String),
        ];
        let tree = build_tree(&keys);
        assert!(tree.iter().all(|n| n.is_namespace()));
        let labels: Vec<_> = tree.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(labels, vec!["session", "user"]);
    }

    #[test]
    fn leaf_and_namespace_coexist() {
        let keys = vec![
            meta("user", RedisType::String),
            meta("user:1", RedisType::Hash),
        ];
        let tree = build_tree(&keys);
        assert_eq!(tree.len(), 1);
        let user_node = &tree[0];
        assert_eq!(user_node.label, "user");
        assert!(user_node.leaf_type.is_some());
        assert_eq!(user_node.children.len(), 1);
        assert_eq!(user_node.children[0].label, "1");
    }

    #[test]
    fn skip_empty_segments() {
        let keys = vec![
            meta("good:key", RedisType::String),
            meta("::bad", RedisType::String),
            meta("bad::key", RedisType::String),
        ];
        let tree = build_tree(&keys);
        let labels: Vec<_> = tree.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(labels, vec!["good"]);
    }

    /// SCAN 装载的 bare key（key_type=None）必须仍被识别为叶子：
    /// 右键删除菜单与搜索匹配都依赖 is_key，而非 leaf_type
    #[test]
    #[allow(clippy::expect_used)]
    fn bare_key_is_still_leaf() {
        let keys = vec![KeyMeta::bare("111"), KeyMeta::bare("user:1")];
        let tree = build_tree(&keys);
        let root_111 = tree.iter().find(|n| n.label == "111").expect("有 111 节点");
        assert!(root_111.is_key, "bare key 应被标记为叶子");
        assert!(
            root_111.leaf_type.is_none(),
            "未查询类型时 leaf_type 保持 None"
        );
        let user = tree
            .iter()
            .find(|n| n.label == "user")
            .expect("有 user 节点");
        assert!(!user.is_key, "纯命名空间不是叶子");
        assert!(user.children[0].is_key);
    }

    #[test]
    fn collect_paths() {
        let keys = vec![
            meta("a:b:c", RedisType::String),
            meta("a:d", RedisType::Set),
        ];
        let tree = build_tree(&keys);
        let mut paths = HashSet::new();
        for n in &tree {
            collect_namespace_paths(n, &mut paths);
        }
        assert!(paths.contains("a"));
        assert!(paths.contains("a:b"));
        assert!(!paths.contains("a:b:c"));
        assert!(!paths.contains("a:d"));
    }

    #[test]
    fn deeply_namespaced_key_is_folded_without_losing_full_key() {
        let key = (0..100)
            .map(|index| format!("n{index}"))
            .collect::<Vec<_>>()
            .join(":");
        let tree = build_tree(&[KeyMeta::bare(key.clone())]);

        let mut depth = 0;
        let mut node = &tree[0];
        let mut rebuilt = Vec::new();
        loop {
            depth += 1;
            rebuilt.push(node.label.as_str());
            if node.children.is_empty() {
                break;
            }
            node = &node.children[0];
        }
        assert_eq!(depth, MAX_NAMESPACE_DEPTH);
        assert_eq!(rebuilt.join(":"), key);
        assert!(node.is_key);
    }
}
