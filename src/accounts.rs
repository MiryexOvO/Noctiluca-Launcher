pub mod device;

use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result, anyhow, ensure};
use mc_launcher_core::{account::Account, auth::microsoft_account as msa};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::library::atomic_write;

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    #[default]
    Microsoft,
    ThirdParty,
    Offline,
}
impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Microsoft => "Microsoft",
            Self::ThirdParty => "第三方",
            Self::Offline => "离线",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Player {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: Kind,
    #[serde(default)]
    pub(crate) client: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skin: Option<String>,
}
impl Player {
    pub fn key(&self) -> String {
        match self.kind {
            Kind::Microsoft => self.id.clone(),
            Kind::ThirdParty => format!("external:{}:{}", self.client, self.id),
            Kind::Offline => format!("offline:{}", self.id),
        }
    }
    pub fn offline(name: &str) -> Result<Self> {
        use md5::{Digest, Md5};
        let name = name.trim();
        ensure!(
            (1..=16).contains(&name.len())
                && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'),
            "离线角色名应为 1–16 位英文字母、数字或下划线"
        );
        let id = uuid::Builder::from_md5_bytes(Md5::digest(format!("OfflinePlayer:{name}")).into())
            .into_uuid();
        Ok(Self {
            id: id.simple().to_string(),
            name: name.into(),
            kind: Kind::Offline,
            client: String::new(),
            skin: None,
        })
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct Accounts {
    pub players: Vec<Player>,
    pub selected: Option<String>,
}

impl Accounts {
    pub fn read(root: &Path) -> Result<Self> {
        match fs::read(root.join("accounts.json")) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).context("账号配置损坏")?),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err.into()),
        }
    }

    pub fn current(&self) -> Option<&Player> {
        self.players
            .iter()
            .find(|p| Some(p.key().as_str()) == self.selected.as_deref())
    }
}

pub(crate) fn change(
    root: &Path,
    edit: impl FnOnce(&mut Accounts) -> Result<()>,
) -> Result<Accounts> {
    let lock = fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("accounts.lock"))?;
    lock.lock()?;
    let mut data = Accounts::read(root)?;
    edit(&mut data)?;
    atomic_write(
        &root.join("accounts.json"),
        &serde_json::to_vec_pretty(&data)?,
    )?;
    Ok(data)
}

pub fn select(root: &Path, id: Option<String>) -> Result<Accounts> {
    change(root, |data| {
        ensure!(
            id.is_none()
                || data
                    .players
                    .iter()
                    .any(|p| Some(p.key().as_str()) == id.as_deref()),
            "账号已移除"
        );
        data.selected = id;
        Ok(())
    })
}

pub(crate) fn credential(player: &Player) -> Result<keyring::Entry> {
    if player.kind == Kind::ThirdParty {
        return Ok(keyring::Entry::new(
            &format!("Noctiluca/Yggdrasil/{}", player.client),
            &player.id,
        )?);
    }
    // The credential target is persistent storage shared with earlier releases.
    Ok(keyring::Entry::new(
        &format!("MCSTART/{}", player.client),
        &player.id,
    )?)
}

pub fn forget(root: &Path, player: &Player) -> Result<Accounts> {
    change(root, |data| {
        if player.kind != Kind::Offline {
            match credential(player)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(err) => return Err(err.into()),
            }
        }
        data.players.retain(|p| p.key() != player.key());
        if data.selected.as_deref() == Some(player.key().as_str()) {
            data.selected = data.players.first().map(Player::key);
        }
        Ok(())
    })
}

fn normalize_client(value: &str) -> Result<String> {
    let id = Uuid::parse_str(value.trim()).context("此版本的微软登录服务配置无效")?;
    ensure!(!id.is_nil(), "此版本的微软登录服务配置无效");
    Ok(id.to_string())
}

pub fn client_id(root: &Path) -> Result<Option<String>> {
    if let Some(id) = option_env!("NOCTILUCA_CLIENT_ID") {
        return normalize_client(id).map(Some);
    }
    if let Some(id) = crate::setting("CLIENT_ID") {
        return normalize_client(id.to_str().context("微软登录服务配置无效")?).map(Some);
    }
    match fs::read(root.join("microsoft-client.json")) {
        Ok(data) => Ok(Some(normalize_client(
            &serde_json::from_slice::<String>(&data).context("微软登录服务配置无效")?,
        )?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
struct Session {
    skin: Option<String>,
    id: String,
    name: String,
    access_token: String,
    refresh_token: String,
}
#[derive(Deserialize)]
struct Licenses {
    items: Vec<License>,
}
#[derive(Deserialize)]
struct License {
    name: String,
}
impl Licenses {
    fn can_play(&self) -> bool {
        self.items
            .iter()
            .any(|i| matches!(i.name.as_str(), "product_minecraft" | "game_minecraft"))
    }
}
fn minecraft_session(tokens: device::Tokens) -> Result<Session> {
    ensure!(
        !tokens.access_token.is_empty() && !tokens.refresh_token.is_empty(),
        "Microsoft 未签发有效的会话，请重新登录"
    );
    let xbl = msa::authenticate_with_xbl(&tokens.access_token)
        .map_err(|_| anyhow!("Xbox Live 登录失败，请确认该微软账号已创建 Xbox 档案"))?;
    let xsts = msa::authenticate_with_xsts(&xbl.token)
        .context("Xbox 安全令牌授权失败，请检查账号的 Xbox 权限")?;
    let userhash = &xsts
        .display_claimns
        .xui
        .first()
        .context("Xbox 返回的档案缺少用户标识")?
        .uhs;
    let mc = msa::authenticate_with_minecraft(userhash, &xsts.token)
        .context("Minecraft 服务未接受 Xbox 登录，请检查应用访问权限")?;
    ensure!(!mc.access_token.is_empty(), "Minecraft 未签发游戏令牌");
    let licenses: Licenses = crate::download::client()?
        .get("https://api.minecraftservices.com/entitlements/license")
        .query(&[("requestId", Uuid::new_v4().to_string())])
        .bearer_auth(&mc.access_token)
        .send()
        .context("无法连接 Minecraft 游戏授权服务")?
        .error_for_status()
        .context("Minecraft 游戏授权验证失败")?
        .json()
        .context("无法读取 Minecraft 游戏授权")?;
    ensure!(
        licenses.can_play(),
        "此微软账号没有有效的 Minecraft Java 版游戏授权"
    );
    let profile = msa::get_profile(&mc.access_token)
        .map_err(|_| anyhow!("未取得 Java 版角色，请先在 Minecraft 官网创建角色"))?;
    ensure!(
        profile.error.is_none() && Uuid::parse_str(&profile.id).is_ok() && !profile.name.is_empty(),
        "Minecraft 返回了无效的 Java 版角色"
    );
    Ok(Session {
        skin: profile
            .skins
            .into_iter()
            .find(|s| s.state == "ACTIVE")
            .map(|s| s.url),
        id: profile.id,
        name: profile.name,
        access_token: mc.access_token,
        refresh_token: tokens.refresh_token,
    })
}

pub struct MicrosoftLogin {
    player: Player,
    refresh_token: String,
}

impl MicrosoftLogin {
    pub fn save(self, root: &Path) -> Result<Accounts> {
        add(root, self.player, Some(&self.refresh_token))
    }
}

pub fn login(
    client: String,
    cancel: &AtomicBool,
    show: impl FnOnce(device::Prompt) -> Result<()>,
) -> Result<MicrosoftLogin> {
    let tokens = device::authorize(&client, cancel, show)?;
    ensure!(!cancel.load(Ordering::Acquire), "登录已取消");
    let session = minecraft_session(tokens)?;
    ensure!(!cancel.load(Ordering::Relaxed), "登录已取消");
    let player = Player {
        kind: Kind::Microsoft,
        skin: session.skin,
        id: session.id,
        name: session.name,
        client,
    };
    Ok(MicrosoftLogin {
        player,
        refresh_token: session.refresh_token,
    })
}
pub fn add(root: &Path, player: Player, token: Option<&str>) -> Result<Accounts> {
    change(root, |data| {
        if let Some(token) = token {
            credential(&player)?.set_password(token)?;
        }
        data.players.retain(|p| p.key() != player.key());
        data.selected = Some(player.key());
        data.players.push(player);
        Ok(())
    })
}

pub fn identity(player: Option<&Player>) -> Result<Account> {
    let player = player.context("请先添加并选择游戏账号")?;
    if player.kind == Kind::Offline {
        return Ok(Account::Offline {
            username: player.name.clone(),
            uuid: player.id.clone(),
        });
    }
    if player.kind == Kind::ThirdParty {
        return crate::third_party::identity(player);
    }
    let entry = credential(player)?;
    let refresh = entry.get_password().context("未找到登录凭据，请重新登录")?;
    let tokens = device::refresh(&player.client, &refresh)
        .context("微软会话刷新失败，请检查网络或重新登录")?;
    let session = minecraft_session(tokens)?;
    ensure!(session.id == player.id, "登录账号不匹配，请重新登录");
    entry.set_password(&session.refresh_token)?;
    Ok(Account::Microsoft {
        username: session.name,
        uuid: session.id,
        access_token: session.access_token,
    })
}
