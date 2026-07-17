//! 文件路径树：`Vec<FileStatus>` → 目录树 → Row 列表。中间空目录压缩为单行（IDEA compact 风）

use std::collections::{BTreeMap, HashSet};

use ramag_domain::entities::FileStatus;

/// 树节点：目录 / 文件
pub(super) enum Node {
    Dir {
        children: BTreeMap<String, Node>,
        file_count: usize,
    },
    File {
        idx: usize,
    },
}

/// 扁平化后的一行（uniform_list 数据单元，强制等高）
#[derive(Clone)]
pub(super) enum Row {
    Dir {
        display_name: String,
        dir_path: String,
        depth: usize,
        is_collapsed: bool,
        file_count: usize,
    },
    File {
        idx: usize,
        depth: usize,
    },
}

/// 把扁平 file 列表构建成嵌套目录树（按 / 分割路径）
pub(super) fn build_tree(files: &[FileStatus]) -> BTreeMap<String, Node> {
    let mut root = BTreeMap::new();
    for (idx, file) in files.iter().enumerate() {
        insert_path(&mut root, &file.path, idx);
    }
    populate_file_counts(&mut root);
    root
}

pub(super) fn build_tree_for_indices(
    files: &[FileStatus],
    indices: &[usize],
) -> BTreeMap<String, Node> {
    let mut root: BTreeMap<String, Node> = BTreeMap::new();
    for &idx in indices {
        if let Some(file) = files.get(idx) {
            insert_path(&mut root, &file.path, idx);
        }
    }
    populate_file_counts(&mut root);
    root
}

fn insert_path(map: &mut BTreeMap<String, Node>, path: &str, idx: usize) {
    let mut parts = path.split('/').peekable();
    let mut current = map;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            current.insert(part.to_string(), Node::File { idx });
            return;
        }
        let entry = current
            .entry(part.to_string())
            .or_insert_with(|| Node::Dir {
                children: BTreeMap::new(),
                file_count: 0,
            });
        let Node::Dir { children, .. } = entry else {
            return;
        };
        current = children;
    }
}

/// 构树后单次后序遍历缓存子树文件数，避免扁平化时为每个目录重复扫描整棵子树。
fn populate_file_counts(map: &mut BTreeMap<String, Node>) -> usize {
    let mut total = 0usize;
    for node in map.values_mut() {
        total = total.saturating_add(match node {
            Node::Dir {
                children,
                file_count,
            } => {
                *file_count = populate_file_counts(children);
                *file_count
            }
            Node::File { .. } => 1,
        });
    }
    total
}

/// 扁平化树：dir 自动压缩单链中间目录（IDEA compact middle packages）
pub(super) fn flatten(
    map: &BTreeMap<String, Node>,
    depth: usize,
    prefix: &str,
    collapsed: &HashSet<String>,
    out: &mut Vec<Row>,
) {
    let mut dirs: Vec<(String, &BTreeMap<String, Node>, usize)> = Vec::new();
    let mut files: Vec<(String, usize)> = Vec::new();
    for (name, node) in map {
        match node {
            Node::Dir {
                children,
                file_count,
            } => dirs.push((name.clone(), children, *file_count)),
            Node::File { idx } => files.push((name.clone(), *idx)),
        }
    }
    for (name, children, file_count) in dirs {
        let mut display = name.clone();
        let mut full = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let mut cur = children;
        while cur.len() == 1 {
            let Some((
                only_name,
                Node::Dir {
                    children: grandchildren,
                    ..
                },
            )) = cur.iter().next()
            else {
                break;
            };
            display = format!("{display}/{only_name}");
            full = format!("{full}/{only_name}");
            cur = grandchildren;
        }
        let is_collapsed = collapsed.contains(&full);
        out.push(Row::Dir {
            display_name: display,
            dir_path: full.clone(),
            depth,
            is_collapsed,
            file_count,
        });
        if !is_collapsed {
            flatten(cur, depth + 1, &full, collapsed, out);
        }
    }
    for (_name, idx) in files {
        out.push(Row::File { idx, depth });
    }
}

#[cfg(test)]
mod tests {
    use ramag_domain::entities::FileChangeKind;

    use super::*;

    #[test]
    fn deeply_nested_path_is_compacted_without_recursive_insertion() {
        let path = (0..512)
            .map(|index| format!("d{index}"))
            .chain(std::iter::once("file.rs".to_string()))
            .collect::<Vec<_>>()
            .join("/");
        let files = vec![FileStatus {
            path,
            old_path: None,
            staged: Some(FileChangeKind::Added),
            unstaged: None,
        }];

        let tree = build_tree(&files);
        let mut rows = Vec::new();
        flatten(&tree, 0, "", &HashSet::new(), &mut rows);

        assert_eq!(rows.len(), 2);
        assert!(matches!(rows[1], Row::File { idx: 0, .. }));
    }

    #[test]
    fn directory_file_counts_are_computed_once_during_build() {
        let files = ["src/a.rs", "src/nested/b.rs", "tests/test.rs"]
            .into_iter()
            .map(|path| FileStatus {
                path: path.to_string(),
                old_path: None,
                staged: Some(FileChangeKind::Added),
                unstaged: None,
            })
            .collect::<Vec<_>>();

        let tree = build_tree(&files);
        let mut rows = Vec::new();
        flatten(&tree, 0, "", &HashSet::new(), &mut rows);

        let counts = rows
            .iter()
            .filter_map(|row| match row {
                Row::Dir {
                    display_name,
                    file_count,
                    ..
                } => Some((display_name.as_str(), *file_count)),
                Row::File { .. } => None,
            })
            .collect::<Vec<_>>();
        assert!(counts.contains(&("src", 2)));
        assert!(counts.contains(&("nested", 1)));
        assert!(counts.contains(&("tests", 1)));
    }
}
