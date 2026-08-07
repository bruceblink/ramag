//! JumpServer API wire DTO、字段校验与领域模型转换。

use super::*;

#[derive(Deserialize)]
pub(super) struct WireAsset {
    pub(super) id: String,
    #[serde(default)]
    pub(super) org_id: String,
    pub(super) name: String,
    #[serde(default)]
    pub(super) address: String,
    #[serde(default)]
    pub(super) platform: Value,
    #[serde(default)]
    pub(super) labels: Option<Vec<WireLabel>>,
    #[serde(default)]
    pub(super) nodes: Option<Vec<WireReference>>,
    #[serde(default = "default_true")]
    pub(super) is_active: bool,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum WireReference {
    Id(String),
    Object { id: String },
}

impl WireReference {
    pub(super) fn into_id(self) -> String {
        match self {
            Self::Id(id) | Self::Object { id } => id,
        }
    }
}

#[derive(Deserialize)]
pub(super) struct WireLabel {
    #[serde(default)]
    pub(super) name: String,
    #[serde(default)]
    pub(super) value: String,
}

impl WireLabel {
    pub(super) fn into_label(self) -> Result<JumpServerLabel> {
        Ok(JumpServerLabel {
            name: checked_optional_field(self.name)?,
            value: checked_optional_field(self.value)?,
        })
    }
}

impl WireAsset {
    pub(super) fn into_asset(self, fallback_org_id: &str) -> Result<JumpServerAsset> {
        let mut node_ids = self
            .nodes
            .unwrap_or_default()
            .into_iter()
            .map(WireReference::into_id)
            .map(|id| checked_field("资产节点 ID", id))
            .collect::<Result<Vec<_>>>()?;
        node_ids.sort();
        node_ids.dedup();
        let asset = JumpServerAsset {
            id: checked_field("资产 ID", self.id)?,
            org_id: checked_optional_field(if self.org_id.is_empty() {
                fallback_org_id.to_string()
            } else {
                self.org_id
            })?,
            name: checked_field("资产名称", self.name)?,
            address: checked_optional_field(self.address)?,
            platform: checked_optional_field(value_label(&self.platform))?,
            labels: self
                .labels
                .unwrap_or_default()
                .into_iter()
                .map(WireLabel::into_label)
                .collect::<Result<Vec<_>>>()?,
            node_ids,
            favorite: false,
            ungrouped: false,
            active: self.is_active,
        };
        asset
            .validate_id()
            .map_err(|_| DomainError::ConnectionFailed("JumpServer 返回的资产 ID 无效".into()))?;
        Ok(asset)
    }
}

#[derive(Deserialize)]
pub(super) struct WireNode {
    pub(super) id: String,
    #[serde(default)]
    pub(super) org_id: String,
    pub(super) key: String,
    #[serde(default)]
    pub(super) name: String,
    #[serde(default)]
    pub(super) value: String,
    #[serde(default)]
    pub(super) full_value: String,
    #[serde(default)]
    pub(super) assets_amount: usize,
}

impl WireNode {
    pub(super) fn into_node(self, fallback_org_id: &str) -> Result<JumpServerNode> {
        let name = if self.name.trim().is_empty() {
            self.value
        } else {
            self.name
        };
        let full_name = if self.full_value.trim().is_empty() {
            name.clone()
        } else {
            self.full_value
        };
        Ok(JumpServerNode {
            id: checked_field("资产节点 ID", self.id)?,
            org_id: checked_optional_field(if self.org_id.is_empty() {
                fallback_org_id.to_string()
            } else {
                self.org_id
            })?,
            key: checked_field("资产节点 key", self.key)?,
            name: checked_field("资产节点名称", name)?,
            full_name: checked_field("资产节点完整名称", full_name)?,
            assets_amount: self.assets_amount,
        })
    }
}

#[derive(Deserialize)]
pub(super) struct WireAssetDetail {
    #[serde(flatten)]
    pub(super) asset: WireAsset,
    #[serde(default)]
    pub(super) permed_protocols: Vec<Value>,
    #[serde(default)]
    pub(super) permed_accounts: Vec<WireAccount>,
}

impl WireAssetDetail {
    pub(super) fn into_detail(self, fallback_org_id: &str) -> Result<JumpServerAssetDetail> {
        let ssh_enabled = self
            .permed_protocols
            .iter()
            .any(|protocol| protocol_is(protocol, "ssh"));
        let rdp_web_enabled = self.permed_protocols.iter().any(|protocol| {
            protocol_is(protocol, "rdp")
                && protocol
                    .get("public")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
        });
        let accounts = self
            .permed_accounts
            .into_iter()
            .map(WireAccount::into_account)
            .collect::<Result<Vec<_>>>()?;
        Ok(JumpServerAssetDetail {
            asset: self.asset.into_asset(fallback_org_id)?,
            accounts,
            ssh_enabled,
            rdp_web_enabled,
        })
    }
}

pub(super) fn protocol_is(protocol: &Value, expected: &str) -> bool {
    protocol
        .as_str()
        .or_else(|| protocol.get("name").and_then(Value::as_str))
        .is_some_and(|name| name.eq_ignore_ascii_case(expected))
}

#[derive(Deserialize)]
pub(super) struct WireAccount {
    pub(super) id: String,
    #[serde(default)]
    pub(super) alias: String,
    pub(super) name: String,
    #[serde(default)]
    pub(super) username: String,
    pub(super) has_secret: Option<bool>,
    #[serde(default)]
    pub(super) actions: Value,
}

impl WireAccount {
    pub(super) fn into_account(self) -> Result<JumpServerAccount> {
        let id = checked_field("账号 ID", self.id)?;
        let alias = checked_optional_field(self.alias)?;
        let can_connect = self.actions.is_null()
            || self.actions.as_array().is_some_and(|actions| {
                actions.iter().any(|action| {
                    action
                        .as_str()
                        .or_else(|| action.get("value").and_then(Value::as_str))
                        .is_some_and(|name| name.eq_ignore_ascii_case("connect"))
                })
            });
        Ok(JumpServerAccount {
            alias: if alias.is_empty() { id.clone() } else { alias },
            id,
            name: checked_field("账号名称", self.name)?,
            username: checked_optional_field(self.username)?,
            has_secret: self.has_secret.unwrap_or(true),
            can_connect,
        })
    }
}

#[derive(Deserialize)]
pub(super) struct WireConnectionToken {
    pub(super) id: String,
}

#[derive(Deserialize)]
pub(super) struct WireWebEndpoint {
    #[serde(default)]
    pub(super) host: String,
    #[serde(default)]
    pub(super) https_port: u16,
    #[serde(default)]
    pub(super) http_port: u16,
}

pub(super) fn build_rdp_web_session_url(
    base_url: &str,
    endpoint: &WireWebEndpoint,
    token_id: &str,
    asset_name: &str,
) -> Result<String> {
    uuid::Uuid::parse_str(token_id)
        .map_err(|_| DomainError::ConnectionFailed("JumpServer 返回的连接令牌无效".into()))?;
    let base = Url::parse(base_url)
        .map_err(|error| DomainError::InvalidConfig(format!("JumpServer 地址无效：{error}")))?;
    if !matches!(base.scheme(), "http" | "https") {
        return Err(DomainError::InvalidConfig(
            "JumpServer 地址只支持 http 或 https".into(),
        ));
    }

    let host = checked_optional_field(endpoint.host.clone())?;
    let mut url = base.clone();
    url.set_query(None);
    url.set_fragment(None);
    if !host.is_empty() {
        url.set_host(Some(&host)).map_err(|_| {
            DomainError::ConnectionFailed("JumpServer 返回的 Web 会话主机无效".into())
        })?;
    }
    let port = if url.scheme() == "https" {
        endpoint.https_port
    } else {
        endpoint.http_port
    };
    if port != 0 {
        url.set_port(Some(port)).map_err(|_| {
            DomainError::ConnectionFailed("JumpServer 返回的 Web 会话端口无效".into())
        })?;
    }

    let same_origin = url.scheme() == base.scheme()
        && url.host_str() == base.host_str()
        && url.port_or_known_default() == base.port_or_known_default();
    let path = if same_origin {
        format!("{}/lion/connect", base.path().trim_end_matches('/'))
    } else {
        "/lion/connect".into()
    };
    url.set_path(&path);
    url.query_pairs_mut()
        .append_pair("ramag_asset", asset_name)
        .append_pair("token", token_id);
    Ok(url.to_string())
}

pub(super) fn parse_asset_page(bytes: &[u8]) -> Result<(Vec<WireAsset>, Option<usize>, bool)> {
    let value: Value = parse_json(bytes, "获取 JumpServer 资源")?;
    match value {
        Value::Array(items) => Ok((parse_asset_items(items)?, None, false)),
        Value::Object(mut object) => {
            let count = object
                .get("count")
                .and_then(Value::as_u64)
                .and_then(|count| usize::try_from(count).ok());
            let items = object
                .remove("results")
                .or_else(|| object.remove("data"))
                .and_then(|items| items.as_array().cloned())
                .ok_or_else(|| {
                    DomainError::ConnectionFailed("JumpServer 资源列表格式无效".into())
                })?;
            Ok((parse_asset_items(items)?, count, true))
        }
        _ => Err(DomainError::ConnectionFailed(
            "JumpServer 资源列表格式无效".into(),
        )),
    }
}

pub(super) fn parse_asset_items(items: Vec<Value>) -> Result<Vec<WireAsset>> {
    items
        .into_iter()
        .map(|item| {
            serde_json::from_value(item).map_err(|error| {
                DomainError::ConnectionFailed(format!("JumpServer 资源字段无效：{error}"))
            })
        })
        .collect()
}

pub(super) fn parse_node_page(bytes: &[u8]) -> Result<(Vec<WireNode>, Option<usize>, bool)> {
    let value: Value = parse_json(bytes, "获取 JumpServer 资产树")?;
    match value {
        Value::Array(items) => Ok((parse_node_items(items)?, None, false)),
        Value::Object(mut object) => {
            let count = object
                .get("count")
                .and_then(Value::as_u64)
                .and_then(|count| usize::try_from(count).ok());
            let items = object
                .remove("results")
                .or_else(|| object.remove("data"))
                .and_then(|items| items.as_array().cloned())
                .ok_or_else(|| DomainError::ConnectionFailed("JumpServer 资产树格式无效".into()))?;
            Ok((parse_node_items(items)?, count, true))
        }
        _ => Err(DomainError::ConnectionFailed(
            "JumpServer 资产树格式无效".into(),
        )),
    }
}

pub(super) fn parse_node_items(items: Vec<Value>) -> Result<Vec<WireNode>> {
    items
        .into_iter()
        .map(|item| {
            serde_json::from_value(item).map_err(|error| {
                DomainError::ConnectionFailed(format!("JumpServer 资产节点字段无效：{error}"))
            })
        })
        .collect()
}

pub(super) fn checked_field(label: &str, value: String) -> Result<String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(DomainError::ConnectionFailed(format!(
            "JumpServer 返回的{label}为空"
        )));
    }
    checked_optional_field(value)
}

pub(super) fn checked_optional_field(value: String) -> Result<String> {
    if value.len() > MAX_API_FIELD_BYTES || value.contains(['\r', '\n']) {
        return Err(DomainError::ConnectionFailed(
            "JumpServer 返回了过长或包含换行的字段".into(),
        ));
    }
    Ok(value)
}

pub(super) fn value_label(value: &Value) -> String {
    value
        .as_str()
        .or_else(|| value.get("name").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

pub(super) fn default_true() -> bool {
    true
}
