use std::collections::HashMap;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, button::ButtonVariants as _,
    h_flex, v_flex,
};
use ramag_domain::entities::{JumpServerNode, JumpServerOrganization};

use super::jumpserver_dialog::{
    JumpServerPanel, JumpServerTreeSelection, node_is_root, tree_node_identity,
    tree_organization_identity,
};
use super::render_jumpserver_dialog::ASSET_PANE_HEIGHT;

const TREE_WIDTH: f32 = 230.0;
const TREE_ROW_HEIGHT: f32 = 31.0;

#[derive(Clone)]
struct VisibleNode {
    node: JumpServerNode,
    depth: usize,
    has_children: bool,
}

impl JumpServerPanel {
    pub(super) fn render_asset_tree(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let mut body = v_flex()
            .id("jumpserver-asset-tree-body")
            .debug_selector(|| "jumpserver-asset-tree-body".into())
            .w_full()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(5.0));

        let organizations = self.tree_organizations();
        if self.nodes.is_empty() || organizations.is_empty() {
            body = body.child(self.render_all_tree_row(cx));
        } else {
            if organizations.len() <= 1 {
                let org_id = organizations
                    .first()
                    .map_or("", |organization| organization.id.as_str());
                for (index, row) in visible_nodes(&self.nodes, org_id, &self.expanded_tree_items, 0)
                    .into_iter()
                    .enumerate()
                {
                    body = body.child(self.render_tree_node(index, row, cx));
                }
            } else {
                let mut index = 0usize;
                body = body.child(self.render_all_tree_row(cx));
                for organization in organizations {
                    body = body.child(self.render_tree_organization(&organization, cx));
                    if self
                        .expanded_tree_items
                        .contains(&tree_organization_identity(&organization.id))
                    {
                        for row in visible_nodes(
                            &self.nodes,
                            &organization.id,
                            &self.expanded_tree_items,
                            1,
                        ) {
                            body = body.child(self.render_tree_node(index, row, cx));
                            index = index.saturating_add(1);
                        }
                    }
                }
            }
        }

        v_flex()
            .id("jumpserver-asset-tree")
            .debug_selector(|| "jumpserver-asset-tree".into())
            .w(px(TREE_WIDTH))
            .h(px(ASSET_PANE_HEIGHT))
            .flex_none()
            .border_1()
            .border_color(border)
            .rounded(px(7.0))
            .overflow_hidden()
            .child(
                h_flex()
                    .w_full()
                    .h(px(38.0))
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .px(px(10.0))
                    .border_b_1()
                    .border_color(border)
                    .bg(cx.theme().secondary)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("资产树"),
                    )
                    .child(
                        ramag_ui::clickable_button("refresh-jumpserver-tree")
                            .ghost()
                            .xsmall()
                            .icon(ramag_ui::icons::refresh_cw())
                            .tooltip("刷新资源")
                            .disabled(self.is_busy())
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.load_assets(cx);
                            })),
                    ),
            )
            .child(body)
    }

    fn tree_organizations(&self) -> Vec<JumpServerOrganization> {
        let mut organizations = Vec::new();
        if let Some(session) = &self.session {
            for organization in &session.organizations {
                if self.nodes.iter().any(|node| node.org_id == organization.id) {
                    organizations.push(organization.clone());
                }
            }
        }
        for node in self.nodes.iter() {
            if organizations
                .iter()
                .any(|organization| organization.id == node.org_id)
            {
                continue;
            }
            organizations.push(JumpServerOrganization {
                id: node.org_id.clone(),
                name: if node.org_id.is_empty() {
                    "默认组织".into()
                } else {
                    node.org_id.clone()
                },
            });
        }
        organizations.retain(|organization| {
            self.assets
                .iter()
                .any(|asset| asset.org_id == organization.id)
        });
        organizations
    }

    fn render_all_tree_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected_tree_item == JumpServerTreeSelection::All;
        let mut selected_bg = cx.theme().accent;
        selected_bg.a = 0.14;
        h_flex()
            .id("jumpserver-tree-flat-assets")
            .debug_selector(|| "jumpserver-tree-flat-assets".into())
            .w_full()
            .h(px(TREE_ROW_HEIGHT))
            .items_center()
            .gap(px(6.0))
            .px(px(7.0))
            .rounded(px(5.0))
            .when(selected, |row| row.bg(selected_bg))
            .when(!selected && !self.is_busy(), |row| {
                row.hover(|row| row.bg(cx.theme().muted))
            })
            .when(!self.is_busy(), |row| {
                row.cursor_pointer()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.select_all_assets(cx);
                    }))
            })
            .child(Icon::new(IconName::FolderOpen).small())
            .child(div().flex_1().min_w_0().text_sm().child("全部资源"))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.assets.len().to_string()),
            )
            .into_any_element()
    }

    fn render_tree_organization(
        &self,
        organization: &JumpServerOrganization,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let expanded = self
            .expanded_tree_items
            .contains(&tree_organization_identity(&organization.id));
        let selected = self.selected_tree_item
            == JumpServerTreeSelection::Organization(organization.id.clone());
        let org_id = organization.id.clone();
        let org_id_for_toggle = org_id.clone();
        let count = self
            .assets
            .iter()
            .filter(|asset| asset.org_id == organization.id)
            .count();
        let mut selected_bg = cx.theme().accent;
        selected_bg.a = 0.14;

        h_flex()
            .id(SharedString::from(format!(
                "jumpserver-tree-organization-{}",
                organization.id
            )))
            .w_full()
            .h(px(TREE_ROW_HEIGHT))
            .items_center()
            .gap(px(4.0))
            .px(px(4.0))
            .rounded(px(5.0))
            .when(selected, |row| row.bg(selected_bg))
            .when(!selected && !self.is_busy(), |row| {
                row.hover(|row| row.bg(cx.theme().muted))
            })
            .when(!self.is_busy(), |row| {
                row.cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.select_tree_organization(org_id.clone(), cx);
                    }))
            })
            .child(
                div()
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        ramag_ui::clickable_button(SharedString::from(format!(
                            "jumpserver-tree-organization-toggle-{}",
                            organization.id
                        )))
                        .ghost()
                        .xsmall()
                        .icon(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .disabled(self.is_busy())
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                this.toggle_tree_organization(org_id_for_toggle.clone(), cx);
                            },
                        )),
                    ),
            )
            .child(Icon::new(if expanded {
                IconName::FolderOpen
            } else {
                IconName::FolderClosed
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(organization.name.clone()),
            )
            .child(
                div()
                    .pr(px(4.0))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(count.to_string()),
            )
            .into_any_element()
    }

    fn render_tree_node(
        &self,
        index: usize,
        row: VisibleNode,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.selected_tree_item
            == (JumpServerTreeSelection::Node {
                org_id: row.node.org_id.clone(),
                node_id: row.node.id.clone(),
            });
        let identity = tree_node_identity(&row.node.org_id, &row.node.id);
        let expanded = self.expanded_tree_items.contains(&identity);
        let org_id = row.node.org_id.clone();
        let node_id = row.node.id.clone();
        let org_id_for_toggle = org_id.clone();
        let node_id_for_toggle = node_id.clone();
        let mut selected_bg = cx.theme().accent;
        selected_bg.a = 0.14;
        let icon = if row.node.is_favorite() {
            IconName::Star
        } else if expanded {
            IconName::FolderOpen
        } else {
            IconName::FolderClosed
        };

        h_flex()
            .id(SharedString::from(format!("jumpserver-tree-node-{index}")))
            .debug_selector(move || format!("jumpserver-tree-node-{index}"))
            .w_full()
            .h(px(TREE_ROW_HEIGHT))
            .items_center()
            .gap(px(4.0))
            .pl(px(4.0 + row.depth as f32 * 15.0))
            .pr(px(7.0))
            .rounded(px(5.0))
            .when(selected, |item| item.bg(selected_bg))
            .when(!selected && !self.is_busy(), |item| {
                item.hover(|item| item.bg(cx.theme().muted))
            })
            .when(!self.is_busy(), |item| {
                item.cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.select_tree_node(org_id.clone(), node_id.clone(), cx);
                    }))
            })
            .child(if row.has_children {
                div()
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        ramag_ui::clickable_button(SharedString::from(format!(
                            "jumpserver-tree-toggle-{index}"
                        )))
                        .ghost()
                        .xsmall()
                        .icon(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .disabled(self.is_busy())
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                this.toggle_tree_node(
                                    org_id_for_toggle.clone(),
                                    node_id_for_toggle.clone(),
                                    cx,
                                );
                            },
                        )),
                    )
                    .into_any_element()
            } else {
                div().w(px(24.0)).into_any_element()
            })
            .child(
                Icon::new(icon)
                    .small()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(row.node.name),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(row.node.assets_amount.to_string()),
            )
            .into_any_element()
    }
}

fn visible_nodes(
    nodes: &[JumpServerNode],
    org_id: &str,
    expanded: &std::collections::HashSet<String>,
    base_depth: usize,
) -> Vec<VisibleNode> {
    let org_nodes = nodes
        .iter()
        .filter(|node| node.org_id == org_id)
        .collect::<Vec<_>>();
    let mut children: HashMap<&str, Vec<&JumpServerNode>> = HashMap::new();
    for node in &org_nodes {
        if !node_is_root(nodes, node) {
            children.entry(node.parent_key()).or_default().push(node);
        }
    }
    for siblings in children.values_mut() {
        sort_nodes(siblings);
    }
    let mut roots = org_nodes
        .into_iter()
        .filter(|node| node_is_root(nodes, node))
        .collect::<Vec<_>>();
    sort_nodes(&mut roots);

    let mut stack = roots
        .into_iter()
        .rev()
        .map(|node| (node, base_depth))
        .collect::<Vec<_>>();
    let mut visible = Vec::new();
    while let Some((node, depth)) = stack.pop() {
        let node_children = children.get(node.key.as_str());
        visible.push(VisibleNode {
            node: node.clone(),
            depth,
            has_children: node_children.is_some_and(|items| !items.is_empty()),
        });
        if expanded.contains(&tree_node_identity(&node.org_id, &node.id))
            && let Some(node_children) = node_children
        {
            for child in node_children.iter().rev() {
                stack.push((child, depth.saturating_add(1)));
            }
        }
    }
    visible
}

fn sort_nodes(nodes: &mut Vec<&JumpServerNode>) {
    nodes.sort_by(|left, right| {
        node_order(left)
            .cmp(&node_order(right))
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
}

fn node_order(node: &JumpServerNode) -> u8 {
    if node.is_favorite() {
        0
    } else if node.is_ungrouped() {
        1
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, key: &str, name: &str) -> JumpServerNode {
        JumpServerNode {
            id: id.into(),
            org_id: "org-1".into(),
            key: key.into(),
            name: name.into(),
            full_name: name.into(),
            assets_amount: 1,
        }
    }

    #[test]
    fn visible_tree_keeps_favorite_first_and_hides_collapsed_children() {
        let nodes = vec![
            node("child", "1:2", "工业仿真"),
            node("root", "1", "DEFAULT"),
            node("favorite", "favorite", "收藏夹"),
        ];
        let collapsed = visible_nodes(&nodes, "org-1", &Default::default(), 0);
        assert_eq!(
            collapsed
                .iter()
                .map(|row| row.node.id.as_str())
                .collect::<Vec<_>>(),
            vec!["favorite", "root"]
        );

        let expanded = std::collections::HashSet::from([tree_node_identity("org-1", "root")]);
        let visible = visible_nodes(&nodes, "org-1", &expanded, 0);
        assert_eq!(visible.len(), 3);
        assert_eq!(visible[2].node.id, "child");
        assert_eq!(visible[2].depth, 1);
    }
}
