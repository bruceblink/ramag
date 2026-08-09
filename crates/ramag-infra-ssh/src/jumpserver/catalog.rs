//! JumpServer 组织、节点、资产和远程桌面编排。

use super::*;

pub(super) async fn fetch_workbench_organizations(
    client: &Client,
    session: &JumpServerSession,
) -> Result<Vec<JumpServerOrganization>> {
    let url = api_url(&session.base_url, "api/v1/users/profile/permissions/")?;
    let response = authorized_request(client.get(url), session, None)?
        .send()
        .await
        .map_err(|error| request_error("获取 JumpServer 组织权限", error))?;
    // 较老版本可能没有该接口，此时继续使用默认组织读取资源。
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    let body = response_body(response, "获取 JumpServer 组织权限").await?;
    let permissions: UserPermissions = parse_json(&body, "获取 JumpServer 组织权限")?;
    validate_organizations(permissions.workbench_orgs)
}

pub(super) fn validate_organizations(
    organizations: Vec<LoginOrganization>,
) -> Result<Vec<JumpServerOrganization>> {
    let mut seen = HashSet::new();
    let mut validated = Vec::new();
    for organization in organizations {
        let id = checked_field("组织 ID", organization.id)?;
        HeaderValue::from_str(&id)
            .map_err(|_| DomainError::ConnectionFailed("JumpServer 组织 ID 无效".into()))?;
        if seen.insert(id.clone()) {
            validated.push(JumpServerOrganization {
                id,
                name: checked_field("组织名称", organization.name)?,
            });
        }
    }
    Ok(validated)
}

pub(super) async fn load_catalog(
    client: &Client,
    session: &JumpServerSession,
) -> Result<JumpServerCatalog> {
    let nodes = list_nodes(client, session).await?;
    let mut assets = list_assets(client, session).await?;
    if !nodes.is_empty() {
        mark_special_assets(client, session, &nodes, &mut assets, "favorite").await?;
        mark_special_assets(client, session, &nodes, &mut assets, "ungrouped").await?;
    }
    Ok(JumpServerCatalog { assets, nodes })
}

async fn list_nodes(client: &Client, session: &JumpServerSession) -> Result<Vec<JumpServerNode>> {
    let organizations: Vec<Option<JumpServerOrganization>> = if session.organizations.is_empty() {
        vec![None]
    } else {
        session.organizations.iter().cloned().map(Some).collect()
    };
    let mut nodes = Vec::new();
    let mut seen = HashSet::new();
    for organization in organizations {
        let org_id = organization.as_ref().map(|org| org.id.as_str());
        if !list_organization_nodes(client, session, org_id, &mut nodes, &mut seen).await? {
            tracing::info!(
                operation = "jumpserver_catalog_load",
                fallback = "flat_asset_list",
                "JumpServer node API unavailable; using flat asset list"
            );
            return Ok(Vec::new());
        }
    }
    nodes.sort_by(|left, right| {
        left.org_id
            .cmp(&right.org_id)
            .then_with(|| special_node_order(left).cmp(&special_node_order(right)))
            .then_with(|| left.key.cmp(&right.key))
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    Ok(nodes)
}

async fn list_organization_nodes(
    client: &Client,
    session: &JumpServerSession,
    org_id: Option<&str>,
    nodes: &mut Vec<JumpServerNode>,
    seen: &mut HashSet<(String, String)>,
) -> Result<bool> {
    let mut offset = 0usize;
    let mut pages = 0usize;
    loop {
        let mut url = api_url(&session.base_url, "api/v1/perms/users/self/nodes/")?;
        url.query_pairs_mut()
            .append_pair("limit", &NODE_PAGE_SIZE.to_string())
            .append_pair("offset", &offset.to_string());
        let response = authorized_request(client.get(url), session, org_id)?
            .send()
            .await
            .map_err(|error| request_error("获取 JumpServer 资产树", error))?;
        if matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ) {
            return Ok(false);
        }
        let body = response_body(response, "获取 JumpServer 资产树").await?;
        let (wire_nodes, count, paged) = parse_node_page(&body)?;
        if count.is_some_and(|total| total > MAX_JUMPSERVER_NODES) {
            return Err(DomainError::Other(format!(
                "JumpServer 资产节点超过 {MAX_JUMPSERVER_NODES} 条上限，请缩小授权范围"
            )));
        }
        let page_len = wire_nodes.len();
        let seen_before = seen.len();
        for wire in wire_nodes {
            let node = wire.into_node(org_id.unwrap_or_default())?;
            let identity = (node.org_id.clone(), node.id.clone());
            if seen.insert(identity) {
                if nodes.len() >= MAX_JUMPSERVER_NODES {
                    return Err(DomainError::Other(format!(
                        "JumpServer 资产节点超过 {MAX_JUMPSERVER_NODES} 条上限，请缩小授权范围"
                    )));
                }
                nodes.push(node);
            }
        }
        offset = offset.saturating_add(page_len);
        pages = pages.saturating_add(1);
        if page_len == 0 || !paged || count.is_some_and(|total| offset >= total) {
            break;
        }
        if seen.len() == seen_before || pages >= MAX_NODE_PAGES {
            return Err(DomainError::ConnectionFailed(
                "JumpServer 资产树分页异常，请稍后重试".into(),
            ));
        }
    }
    Ok(true)
}

async fn mark_special_assets(
    client: &Client,
    session: &JumpServerSession,
    nodes: &[JumpServerNode],
    assets: &mut [JumpServerAsset],
    special_key: &str,
) -> Result<()> {
    let organizations: Vec<Option<String>> = if session.organizations.is_empty() {
        vec![None]
    } else {
        session
            .organizations
            .iter()
            .map(|organization| Some(organization.id.clone()))
            .collect()
    };
    for organization in organizations {
        let org_id = organization.as_deref();
        let has_special_assets = nodes.iter().any(|node| {
            node.org_id == org_id.unwrap_or_default()
                && node.key == special_key
                && node.assets_amount > 0
        });
        if !has_special_assets {
            continue;
        }
        let path = format!("api/v1/perms/users/self/nodes/{special_key}/assets/");
        let mut special_assets = Vec::new();
        let mut seen = HashSet::new();
        list_organization_assets(
            client,
            session,
            org_id,
            &path,
            &mut special_assets,
            &mut seen,
        )
        .await?;
        for asset in assets
            .iter_mut()
            .filter(|asset| asset.org_id == org_id.unwrap_or_default() && seen.contains(&asset.id))
        {
            if special_key == "favorite" {
                asset.favorite = true;
            } else {
                asset.ungrouped = true;
            }
        }
    }
    Ok(())
}

fn special_node_order(node: &JumpServerNode) -> u8 {
    if node.is_favorite() {
        0
    } else if node.is_ungrouped() {
        1
    } else {
        2
    }
}

async fn list_assets(client: &Client, session: &JumpServerSession) -> Result<Vec<JumpServerAsset>> {
    let organizations: Vec<Option<JumpServerOrganization>> = if session.organizations.is_empty() {
        vec![None]
    } else {
        session.organizations.iter().cloned().map(Some).collect()
    };
    let mut assets = Vec::new();
    let mut seen = HashSet::new();
    for organization in organizations {
        let org_id = organization.as_ref().map(|org| org.id.as_str());
        list_organization_assets(
            client,
            session,
            org_id,
            "api/v1/perms/users/self/assets/",
            &mut assets,
            &mut seen,
        )
        .await?;
    }
    assets.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.address.cmp(&right.address))
    });
    Ok(assets)
}

async fn list_organization_assets(
    client: &Client,
    session: &JumpServerSession,
    org_id: Option<&str>,
    path: &str,
    assets: &mut Vec<JumpServerAsset>,
    seen: &mut HashSet<String>,
) -> Result<()> {
    let mut offset = 0usize;
    let mut pages = 0usize;
    loop {
        let mut url = api_url(&session.base_url, path)?;
        url.query_pairs_mut()
            .append_pair("limit", &ASSET_PAGE_SIZE.to_string())
            .append_pair("offset", &offset.to_string());
        let response = authorized_request(client.get(url), session, org_id)?
            .send()
            .await
            .map_err(|error| request_error("获取 JumpServer 资源", error))?;
        let body = response_body(response, "获取 JumpServer 资源").await?;
        let (wire_assets, count, paged) = parse_asset_page(&body)?;
        if count.is_some_and(|total| total > MAX_JUMPSERVER_ASSETS) {
            return Err(DomainError::Other(format!(
                "JumpServer 资源超过 {MAX_JUMPSERVER_ASSETS} 条上限，请缩小授权范围"
            )));
        }
        let page_len = wire_assets.len();
        let seen_before = seen.len();
        for wire in wire_assets {
            let asset = wire.into_asset(org_id.unwrap_or_default())?;
            if seen.insert(asset.id.clone()) {
                if assets.len() >= MAX_JUMPSERVER_ASSETS {
                    return Err(DomainError::Other(format!(
                        "JumpServer 资源超过 {MAX_JUMPSERVER_ASSETS} 条上限，请缩小授权范围"
                    )));
                }
                assets.push(asset);
            }
        }
        offset = offset.saturating_add(page_len);
        pages = pages.saturating_add(1);
        if page_len == 0 || !paged || count.is_some_and(|total| offset >= total) {
            break;
        }
        if seen.len() == seen_before || pages >= MAX_ASSET_PAGES {
            return Err(DomainError::ConnectionFailed(
                "JumpServer 资源分页异常，请稍后重试".into(),
            ));
        }
    }
    Ok(())
}

pub(super) async fn asset_detail(
    client: &Client,
    session: &JumpServerSession,
    selected: &JumpServerAsset,
) -> Result<JumpServerAssetDetail> {
    selected.validate_id().map_err(DomainError::InvalidConfig)?;
    let path = format!("api/v1/perms/users/self/assets/{}/", selected.id);
    let url = api_url(&session.base_url, &path)?;
    let response = authorized_request(client.get(url), session, Some(&selected.org_id))?
        .send()
        .await
        .map_err(|error| request_error("获取资产连接信息", error))?;
    let body = response_body(response, "获取资产连接信息").await?;
    let wire: WireAssetDetail = parse_json(&body, "获取资产连接信息")?;
    let detail = wire.into_detail(&selected.org_id)?;
    if detail.asset.id != selected.id {
        return Err(DomainError::ConnectionFailed(
            "JumpServer 返回了不匹配的资产".into(),
        ));
    }
    Ok(detail)
}

pub(super) async fn create_rdp_web_session(
    client: &Client,
    session: &JumpServerSession,
    asset: &JumpServerAsset,
    account: &JumpServerAccount,
) -> Result<String> {
    asset.validate_id().map_err(DomainError::InvalidConfig)?;
    account
        .validate_for_web_session()
        .map_err(DomainError::InvalidConfig)?;

    let url = api_url(&session.base_url, "api/v1/authentication/connection-token/")?;
    let response = authorized_request(client.post(url), session, Some(&asset.org_id))?
        .json(&serde_json::json!({
            "asset": asset.id,
            "account": account.alias,
            "protocol": "rdp",
            "input_username": account.username,
            "input_secret": "",
            "input_secret_type": "password",
            "connect_method": "web_gui",
            "connect_options": {},
        }))
        .send()
        .await
        .map_err(|error| request_error("创建 RDP Web 会话", error))?;
    let body = response_body(response, "创建 RDP Web 会话").await?;
    let token: WireConnectionToken = parse_json(&body, "创建 RDP Web 会话")?;
    uuid::Uuid::parse_str(&token.id)
        .map_err(|_| DomainError::ConnectionFailed("JumpServer 返回的连接令牌无效".into()))?;

    let mut endpoint_url = api_url(&session.base_url, "api/v1/terminal/endpoints/smart/")?;
    let scheme = endpoint_url.scheme().to_string();
    endpoint_url
        .query_pairs_mut()
        .append_pair("protocol", &scheme)
        .append_pair("token", &token.id);
    let response = authorized_request(client.get(endpoint_url), session, Some(&asset.org_id))?
        .send()
        .await
        .map_err(|error| request_error("获取 RDP Web 会话端点", error))?;
    let body = response_body(response, "获取 RDP Web 会话端点").await?;
    let endpoint: WireWebEndpoint = parse_json(&body, "获取 RDP Web 会话端点")?;
    build_rdp_web_session_url(&session.base_url, &endpoint, &token.id, &asset.name)
}
