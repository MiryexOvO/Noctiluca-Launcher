use crate::{
    accounts::{self, Accounts, Kind, Player},
    download::{self, Downloads},
};
use anyhow::{Context, Result, ensure};
use mc_launcher_core::{
    account::Account,
    net::download::{Checksum, DownloadTask},
};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use url::Url;
use uuid::Uuid;

fn server(raw: &str) -> Result<Url> {
    let mut u = Url::parse(raw.trim()).context("请输入完整的 Yggdrasil API 地址")?;
    ensure!(
        u.scheme() == "https"
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none(),
        "认证服务须使用 HTTPS API 根地址"
    );
    if !u.path().ends_with('/') {
        u.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("无效的 API 根地址"))?
            .push("");
    }
    Ok(u)
}
#[derive(Clone, Deserialize, serde::Serialize)]
struct Profile {
    id: String,
    name: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Reply {
    access_token: String,
    client_token: String,
    selected_profile: Option<Profile>,
    #[serde(default)]
    available_profiles: Vec<Profile>,
}
fn request(http: &Client, base: &Url, action: &str, body: Value) -> Result<Reply> {
    let result = http
        .post(base.join(action)?)
        .json(&body)
        .send()
        .context("第三方认证服务器连接失败")?;
    ensure!(
        result.status().is_success(),
        "第三方认证服务返回 HTTP {}，请检查 API 地址、登录信息或重新登录",
        result.status()
    );
    let reply: Reply = result.json().context("第三方认证响应无效")?;
    ensure!(
        !reply.access_token.is_empty() && !reply.client_token.is_empty(),
        "认证服务器未签发令牌"
    );
    Ok(reply)
}
fn http() -> Result<Client> {
    Ok(download::client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}
pub fn login(
    root: &Path,
    raw: &str,
    user: &str,
    password: &str,
    character: &str,
) -> Result<Accounts> {
    let base = server(raw)?;
    ensure!(
        !user.trim().is_empty() && !password.is_empty(),
        "请输入此认证站的账号和密码"
    );
    let http = http()?;
    let client = Uuid::new_v4().to_string();
    let mut reply = request(
        &http,
        &base,
        "authserver/authenticate",
        json!({"agent":{"name":"Minecraft","version":1},"username":user.trim(),"password":password,"clientToken":client}),
    )?;
    ensure!(
        reply.client_token == client,
        "第三方认证服务返回的客户端令牌不匹配"
    );
    if reply.selected_profile.is_none() {
        let candidates = &reply.available_profiles;
        let profile = if character.trim().is_empty() && candidates.len() == 1 {
            candidates.first()
        } else {
            candidates.iter().find(|p| p.name == character.trim())
        }
        .with_context(|| {
            format!(
                "请填写要使用的角色名。可用角色：{}",
                candidates
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join("、")
            )
        })?;
        reply = request(
            &http,
            &base,
            "authserver/refresh",
            json!({"accessToken":reply.access_token,"clientToken":client,"selectedProfile":profile}),
        )?;
    }
    let profile = reply.selected_profile.context("此认证站账号没有可用角色")?;
    ensure!(
        character.trim().is_empty() || profile.name == character.trim(),
        "认证站返回了其他角色，请检查所选角色名"
    );
    let id = Uuid::parse_str(&profile.id)?.simple().to_string();
    let player = Player {
        id,
        name: profile.name,
        kind: Kind::ThirdParty,
        client: base.to_string(),
        skin: None,
    };
    accounts::add(
        root,
        player,
        Some(&serde_json::to_string(&(
            reply.access_token,
            reply.client_token,
        ))?),
    )
}
pub fn identity(player: &Player) -> Result<Account> {
    let key = accounts::credential(player)?;
    let (access, client): (String, String) =
        serde_json::from_str(&key.get_password().context("请重新登录第三方账号")?)?;
    let reply = request(
        &http()?,
        &server(&player.client)?,
        "authserver/refresh",
        json!({"accessToken":access,"clientToken":client}),
    )?;
    let p = reply.selected_profile.context("认证站未返回已选角色")?;
    ensure!(
        Uuid::parse_str(&p.id)? == Uuid::parse_str(&player.id)? && reply.client_token == client,
        "认证角色或客户端不匹配"
    );
    key.set_password(&serde_json::to_string(&(&reply.access_token, client))?)?;
    Ok(Account::Microsoft {
        username: p.name,
        uuid: p.id,
        access_token: reply.access_token,
    })
}
pub fn agent(shared: &Path, player: &Player, downloads: &Downloads) -> Result<String> {
    let url = server(&player.client)?;
    let jar = shared.join("authlib-injector/authlib-injector-1.2.8.jar");
    downloads.batch(
        shared,
        "第三方认证组件",
        vec![DownloadTask {
            url: "https://authlib-injector.yushi.moe/artifact/56/authlib-injector-1.2.8.jar".into(),
            destination: jar.clone(),
            checksum: Some(Checksum::Sha256(
                "9c7f4343e6c82034958ffb48c14a2cb0c85928be7283103ce17da00c6d5a7b10".into(),
            )),
            label: "authlib-injector 1.2.8".into(),
        }],
    )?;
    Ok(format!("-javaagent:{}={url}", jar.display()))
}
