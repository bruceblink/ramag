//! SSH 配置表单字段与领域模型转换。

use gpui::{AppContext as _, Context, Entity, Window};
use gpui_component::input::InputState;
use ramag_domain::entities::{
    MAX_SSH_HOST_BYTES, MAX_SSH_PATH_BYTES, MAX_SSH_PROFILE_NAME_BYTES, MAX_SSH_USERNAME_BYTES,
    SshAuthMode, SshProfile, SshProfileId,
};

const MAX_PORT_TEXT_BYTES: usize = 5;
const MAX_COLOR_TEXT_BYTES: usize = 7;

fn bounded_input(
    max_bytes: usize,
    window: &mut Window,
    cx: &mut Context<InputState>,
) -> InputState {
    InputState::new(window, cx).validate(move |value, _| value.len() <= max_bytes)
}

pub(super) struct ProfileForm {
    pub name: Entity<InputState>,
    pub color: Entity<InputState>,
    pub host: Entity<InputState>,
    pub port: Entity<InputState>,
    pub username: Entity<InputState>,
    pub key_path: Entity<InputState>,
    pub initial_directory: Entity<InputState>,
    pub ssh_path: Entity<InputState>,
}

impl ProfileForm {
    pub fn new<T: 'static>(window: &mut Window, cx: &mut Context<T>) -> Self {
        Self {
            name: cx.new(|cx| {
                bounded_input(MAX_SSH_PROFILE_NAME_BYTES, window, cx).placeholder("例如 production")
            }),
            color: cx.new(|cx| {
                bounded_input(MAX_COLOR_TEXT_BYTES, window, cx)
                    .placeholder("#007ACC")
                    .default_value("#007ACC")
            }),
            host: cx.new(|cx| {
                bounded_input(MAX_SSH_HOST_BYTES, window, cx)
                    .placeholder("主机、IP 或 ~/.ssh/config 别名")
            }),
            port: cx.new(|cx| {
                bounded_input(MAX_PORT_TEXT_BYTES, window, cx)
                    .placeholder("22")
                    .default_value("22")
            }),
            username: cx.new(|cx| {
                bounded_input(MAX_SSH_USERNAME_BYTES, window, cx)
                    .placeholder("留空交给 OpenSSH 配置")
            }),
            key_path: cx.new(|cx| {
                bounded_input(MAX_SSH_PATH_BYTES, window, cx)
                    .placeholder("OpenSSH 私钥绝对路径（不支持 .ppk）")
            }),
            initial_directory: cx.new(|cx| {
                bounded_input(MAX_SSH_PATH_BYTES, window, cx)
                    .placeholder("例如 /home/user；留空使用远端默认目录")
            }),
            ssh_path: cx.new(|cx| {
                bounded_input(MAX_SSH_PATH_BYTES, window, cx)
                    .placeholder("自定义 ssh/ssh.exe 绝对路径（可选）")
            }),
        }
    }

    pub fn inputs(&self) -> Vec<Entity<InputState>> {
        vec![
            self.name.clone(),
            self.color.clone(),
            self.host.clone(),
            self.port.clone(),
            self.username.clone(),
            self.key_path.clone(),
            self.initial_directory.clone(),
            self.ssh_path.clone(),
        ]
    }

    pub fn set_profile<T: 'static>(
        &self,
        profile: Option<&SshProfile>,
        window: &mut Window,
        cx: &mut Context<T>,
    ) {
        let profile = profile.cloned().unwrap_or_else(|| SshProfile::new("", ""));
        let values = [
            (&self.name, profile.name),
            (&self.color, profile.color),
            (&self.host, profile.host),
            (&self.port, profile.port.to_string()),
            (&self.username, profile.username),
            (&self.key_path, profile.key_path.unwrap_or_default()),
            (
                &self.initial_directory,
                profile.initial_directory.unwrap_or_default(),
            ),
            (&self.ssh_path, profile.ssh_path.unwrap_or_default()),
        ];
        for (input, value) in values {
            input.update(cx, |state, cx| state.set_value(value.clone(), window, cx));
        }
    }

    pub fn values(&self, cx: &gpui::App) -> Vec<String> {
        self.inputs()
            .iter()
            .map(|input| input.read(cx).value().to_string())
            .collect()
    }

    pub fn to_profile(
        &self,
        id: Option<SshProfileId>,
        auth_mode: SshAuthMode,
        cx: &gpui::App,
    ) -> Result<SshProfile, String> {
        let value = |input: &Entity<InputState>| input.read(cx).value().trim().to_string();
        let port = value(&self.port)
            .parse::<u16>()
            .map_err(|_| "SSH 端口必须是 1 - 65535 的整数".to_string())?;
        let optional = |value: String| (!value.is_empty()).then_some(value);
        let profile = SshProfile {
            id: id.unwrap_or_default(),
            name: value(&self.name),
            color: value(&self.color),
            host: value(&self.host),
            port,
            username: value(&self.username),
            auth_mode,
            key_path: (auth_mode == SshAuthMode::KeyFile)
                .then(|| optional(value(&self.key_path)))
                .flatten(),
            initial_directory: optional(value(&self.initial_directory)),
            ssh_path: optional(value(&self.ssh_path)),
        };
        profile.validate()?;
        Ok(profile)
    }
}
