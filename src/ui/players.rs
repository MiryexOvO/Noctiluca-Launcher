use super::*;
use crate::accounts::{Kind, Player};
use ely_gpui_component::{buttons::IconButton, theme::ControlSize};
pub(super) struct Profiles {
    add: bool,
    kind: Kind,
    name: Entity<TextInput>,
    server: Entity<TextInput>,
    username: Entity<TextInput>,
    password: Entity<TextInput>,
    avatar: Option<PathBuf>,
    ticket: u64,
    rename: bool,
}
fn default_face() -> Div {
    let palette = [0x54475e, 0x76647d, 0xd9b4ac, 0xf1d3c8, 0xf4eff9, 0x514468];
    let mut face = div().flex().flex_col();
    for pixels in [
        "00111100", "01100110", "11233211", "12333321", "34533543", "33322333", "23333332",
        "12222221",
    ] {
        let mut row = div().flex();
        for pixel in pixels.bytes() {
            row = row.child(
                div()
                    .size(px(6.))
                    .bg(rgb(palette[usize::from(pixel - b'0')])),
            );
        }
        face = face.child(row);
    }
    face
}

impl Profiles {
    pub fn new(window: &mut Window, cx: &mut Context<Home>) -> Self {
        let mut field = |label: &'static str, hint: &'static str| {
            cx.new(|cx| TextInput::new(window, cx).label(label).placeholder(hint))
        };
        Self {
            add: true,
            kind: Kind::Microsoft,
            name: field("角色名", "游戏中的角色名"),
            server: field("认证服务器", "https://example.org/api/yggdrasil/"),
            username: field("认证站账号", "认证站用户名或邮箱"),
            password: cx.new(|cx| {
                TextInput::new(window, cx)
                    .label("认证站密码")
                    .placeholder("此认证站的密码")
                    .masked()
            }),
            avatar: None,
            ticket: 0,
            rename: false,
        }
    }
}
impl Home {
    pub(super) fn refresh_avatar(&mut self, cx: &mut Context<Self>) {
        self.profiles.ticket += 1;
        let ticket = self.profiles.ticket;
        self.profiles.avatar = None;
        let Some(p) = self.accounts.current().cloned() else {
            return;
        };
        let root = self.root.clone();
        let path = crate::avatar::cache(&root, &p);
        if path.is_file() {
            self.profiles.avatar = Some(path);
            return;
        }
        if p.kind == Kind::Offline {
            return;
        }
        let task = cx
            .background_executor()
            .spawn(async move { crate::avatar::fetch(&root, &p) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |home, cx| {
                if home.profiles.ticket == ticket {
                    home.profiles.avatar = result.ok().flatten();
                    cx.notify();
                }
            });
        })
        .detach();
    }
    fn open_url(&mut self, url: &str, cx: &mut Context<Self>) {
        self.message(open::that(url).map_err(Into::into), cx);
    }
    fn edit_character(&mut self, skin: bool, cx: &mut Context<Self>) {
        let Some(p) = self.accounts.current().cloned() else {
            return;
        };
        match p.kind {
            Kind::Microsoft => self.open_url(
                if skin {
                    "https://www.minecraft.net/msaprofile/mygames/editskin"
                } else {
                    "https://www.minecraft.net/msaprofile/mygames/editprofile"
                },
                cx,
            ),
            Kind::ThirdParty => {
                if let Ok(u) = url::Url::parse(&p.client) {
                    self.open_url(&u.origin().ascii_serialization(), cx);
                }
            }
            Kind::Offline if !skin => {
                self.open_accounts(cx);
                self.profiles.rename = true;
                self.profiles
                    .name
                    .update(cx, |f, cx| f.set_text(p.name, cx));
                cx.notify();
            }
            Kind::Offline => {
                let pick = cx.prompt_for_paths(PathPromptOptions {
                    files: true,
                    directories: false,
                    multiple: false,
                    prompt: Some("选择 64×64 或 64×32 皮肤 PNG".into()),
                });
                cx.spawn(async move |view, cx| {
                    let choice = pick.await;
                    let _ = view.update(cx, |home, cx| {
                        if let Ok(Ok(Some(files))) = choice
                            && let Some(file) = files.first()
                        {
                            let r = crate::avatar::local(&home.root, &p, file);
                            home.account_result(r, cx);
                        }
                    });
                })
                .detach();
            }
        }
    }
    pub(super) fn account_card(&self, busy: bool, cx: &Context<Self>) -> AnyElement {
        let Some(p) = self.accounts.current() else {
            return Button::new("players", "添加账号")
                .icon(IconName::UserPlus)
                .full_width()
                .disabled(busy)
                .on_click(cx.listener(|home, _, _, cx| home.open_accounts(cx)))
                .into_any_element();
        };
        let avatar = div()
            .size(px(48.))
            .rounded_md()
            .overflow_hidden()
            .bg(rgb(SELECTED))
            .flex()
            .items_center()
            .justify_center()
            .child(if let Some(path) = &self.profiles.avatar {
                img(path.clone()).size_full().into_any_element()
            } else {
                default_face().into_any_element()
            });
        let actions = div()
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .child(
                IconButton::new("skin", IconName::Palette)
                    .size(ControlSize::Sm)
                    .tooltip("皮肤与头像")
                    .disabled(busy)
                    .on_click(cx.listener(|h, _, _, cx| h.edit_character(true, cx))),
            )
            .child(
                IconButton::new("change-name", IconName::Pencil)
                    .size(ControlSize::Sm)
                    .tooltip("管理角色名")
                    .disabled(busy)
                    .on_click(cx.listener(|h, _, _, cx| h.edit_character(false, cx))),
            )
            .child(
                IconButton::new("profiles", IconName::Repeat)
                    .size(ControlSize::Sm)
                    .tooltip("切换账号")
                    .disabled(busy)
                    .on_click(cx.listener(|h, _, _, cx| h.open_accounts(cx))),
            )
            .child(
                IconButton::new("add-profile", IconName::UserPlus)
                    .size(ControlSize::Sm)
                    .tooltip("添加账号")
                    .disabled(busy)
                    .on_click(cx.listener(|h, _, _, cx| {
                        h.open_accounts(cx);
                        h.profiles.add = true;
                        cx.notify();
                    })),
            );
        div()
            .flex_none()
            .rounded_lg()
            .bg(rgb(PANEL))
            .p_2()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .child(avatar)
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(p.name.clone()),
            )
            .child(kicker(p.kind.label()))
            .child(actions)
            .into_any_element()
    }
    pub(super) fn open_accounts(&mut self, cx: &mut Context<Self>) {
        self.page = Page::Accounts;
        self.failed = false;
        self.status = "就绪".into();
        self.profiles.rename = false;
        self.profiles.add = self.accounts.players.is_empty();
        self.auth_id = match accounts::client_id(&self.root) {
            Ok(v) => v,
            Err(e) => {
                self.message(Err(e), cx);
                None
            }
        };
        self.auth_edit = self.auth_id.is_none();
        self.auth_input.update(cx, |f, cx| {
            f.set_text(self.auth_id.clone().unwrap_or_default(), cx)
        });
        cx.notify();
    }
    fn account_result(&mut self, result: Result<Accounts>, cx: &mut Context<Self>) {
        match result {
            Ok(v) => {
                self.accounts = v;
                self.failed = false;
                self.status = "账号已更新".into();
                self.profiles.add = self.accounts.players.is_empty();
                self.profiles.rename = false;
                self.refresh_avatar(cx);
            }
            Err(e) => self.message(Err(e), cx),
        }
        cx.notify();
    }
    fn authenticate(&mut self, cx: &mut Context<Self>) {
        if self.logging_in {
            return;
        }
        let name = self.profiles.name.read(cx).text().trim().to_owned();
        if self.profiles.kind == Kind::Offline {
            let r = Player::offline(&name).and_then(|p| accounts::add(&self.root, p, None));
            self.account_result(r, cx);
            return;
        }
        let kind = self.profiles.kind;
        let client = self.auth_id.clone();
        if kind == Kind::Microsoft && client.is_none() {
            self.auth_edit = true;
            cx.notify();
            return;
        }
        let root = self.root.clone();
        let provider = self.profiles.server.read(cx).text().to_owned();
        let user = self.profiles.username.read(cx).text().to_owned();
        let password = self.profiles.password.read(cx).text().to_owned();
        self.profiles
            .password
            .update(cx, |f, cx| f.set_text("", cx));
        let cancel = Arc::new(AtomicBool::new(false));
        self.login_cancel = Some(cancel.clone());
        self.logging_in = true;
        self.failed = false;
        self.status = if kind == Kind::Microsoft {
            "请在浏览器中完成微软登录"
        } else {
            "正在联系认证服务"
        }
        .into();
        let task = cx.background_executor().spawn(async move {
            match client {
                Some(id) if kind == Kind::Microsoft => accounts::login(&root, id, &cancel),
                _ => crate::third_party::login(&root, &provider, &user, &password, &name),
            }
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |h, cx| {
                h.logging_in = false;
                h.login_cancel = None;
                h.account_result(result, cx);
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn account_page(&self, cx: &Context<Self>) -> impl IntoElement {
        let mut page = div().flex().flex_col().gap_3().child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(heading("游戏账号"))
                .child(
                    Button::new("new-profile", "添加账号")
                        .icon(IconName::UserPlus)
                        .disabled(self.logging_in)
                        .on_click(cx.listener(|h, _, _, cx| {
                            h.profiles.add = true;
                            h.profiles.rename = false;
                            cx.notify();
                        })),
                ),
        );
        if self.profiles.rename
            && let Some(player) = self.accounts.current().cloned()
        {
            page = page.child(
                panel("修改离线角色名")
                    .child(Input::new(&self.profiles.name))
                    .child(Button::new("save-name", "保存角色名").on_click(cx.listener(
                        move |h, _, _, cx| {
                            let name = h.profiles.name.read(cx).text().to_owned();
                            let r = Player::offline(&name).and_then(|v| {
                                let mut changed = player.clone();
                                changed.name = v.name;
                                accounts::add(&h.root, changed, None)
                            });
                            h.account_result(r, cx);
                        },
                    ))),
            );
        }
        if self.profiles.add {
            let mut choices = div().flex().gap_2();
            for (i, k) in [Kind::Microsoft, Kind::ThirdParty, Kind::Offline]
                .into_iter()
                .enumerate()
            {
                choices = choices.child(
                    Button::new(("profile-kind", i), k.label())
                        .when(self.profiles.kind == k, |b| b.primary())
                        .disabled(self.logging_in)
                        .on_click(cx.listener(move |h, _, _, cx| {
                            h.profiles.kind = k;
                            h.profiles.name.update(cx, |f, cx| f.set_text("", cx));
                            h.profiles.password.update(cx, |f, cx| f.set_text("", cx));
                            cx.notify();
                        })),
                );
            }
            let mut form = panel("添加账号").child(choices);
            match self.profiles.kind {
                Kind::Microsoft => {
                    if self.auth_edit {
                        form=form.child(kicker("Noctiluca 的 Microsoft Entra 应用程序（客户端）ID")).child(Input::new(&self.auth_input))
                        .child(kicker("桌面应用回调地址  ·  http://localhost/callback"))
                        .child(div().flex().gap_2().child(Button::new("save-client","保存登录配置").on_click(cx.listener(|h,_,_,cx|{
                            match accounts::save_client(&h.root,h.auth_input.read(cx).text()){Ok(id)=>{h.auth_id=Some(id);h.auth_edit=false;h.status="应用 ID 已保存".into();h.failed=false;}Err(e)=>h.message(Err(e),cx)}cx.notify();
                        }))).child(Button::new("client-guide","注册说明").on_click(cx.listener(|h,_,_,cx|h.open_url("https://learn.microsoft.com/entra/identity-platform/quickstart-register-app",cx)))));
                    } else {
                        form = form.child(
                            Button::new("edit-client", "登录配置")
                                .disabled(self.logging_in)
                                .on_click(cx.listener(|h, _, _, cx| {
                                    h.auth_edit = true;
                                    cx.notify();
                                })),
                        );
                    }
                }
                Kind::Offline => {
                    form = form
                        .child(kicker("离线角色名"))
                        .child(Input::new(&self.profiles.name))
                }
                Kind::ThirdParty => {
                    form = form
                        .child(kicker("Yggdrasil 认证服务"))
                        .child(Input::new(&self.profiles.server))
                        .child(Input::new(&self.profiles.username))
                        .child(Input::new(&self.profiles.password))
                        .child(kicker("角色名（多个角色时填写）"))
                        .child(Input::new(&self.profiles.name))
                }
            }
            form = form.child(
                Button::new(
                    "sign-in",
                    match self.profiles.kind {
                        Kind::Offline => "添加离线账号",
                        Kind::Microsoft => "微软正版登录",
                        Kind::ThirdParty => "登录认证站",
                    },
                )
                .primary()
                .disabled(
                    self.logging_in
                        || (self.profiles.kind == Kind::Microsoft
                            && (self.auth_id.is_none() || self.auth_edit)),
                )
                .on_click(cx.listener(|h, _, _, cx| h.authenticate(cx))),
            );
            if self.logging_in && self.profiles.kind == Kind::Microsoft {
                form = form.child(
                    Button::new("cancel-login", "取消登录").on_click(cx.listener(|h, _, _, cx| {
                        if let Some(c) = &h.login_cancel {
                            c.store(true, Ordering::Relaxed);
                        }
                        cx.notify();
                    })),
                );
            }
            page = page.child(form);
        }
        for (i, p) in self.accounts.players.iter().enumerate() {
            let key = p.key();
            let active = self.accounts.selected.as_ref() == Some(&key);
            let remove = p.clone();
            page = page.child(
                panel(&format!("{}  ·  {}", p.name, p.kind.label())).child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new(
                                ("choose-account", i),
                                if active {
                                    "当前账号"
                                } else {
                                    "切换到此账号"
                                },
                            )
                            .disabled(self.logging_in || active)
                            .on_click(cx.listener(
                                move |h, _, _, cx| {
                                    let r = accounts::select(&h.root, Some(key.clone()));
                                    h.account_result(r, cx);
                                },
                            )),
                        )
                        .child(
                            Button::new(("remove-account", i), "移除账号")
                                .disabled(self.logging_in)
                                .on_click(cx.listener(move |h, _, _, cx| {
                                    let r = accounts::forget(&h.root, &remove);
                                    h.account_result(r, cx);
                                })),
                        ),
                ),
            );
        }
        page
    }
}
