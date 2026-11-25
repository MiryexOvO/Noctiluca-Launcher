use super::*;
use crate::accounts::{Kind, Player};
use ely_gpui_component::overlays::Dialog;
use ely_gpui_component::{buttons::IconButton, theme::ControlSize};
pub(super) struct Profiles {
    add: bool,
    kind: Kind,
    name: Entity<TextInput>,
    server: Entity<TextInput>,
    username: Entity<TextInput>,
    client: Option<String>,
    device: Option<accounts::device::Prompt>,
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
            client: None,
            device: None,
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
        self.launch_after_login = None;
        self.page = Page::Accounts;
        self.failed = false;
        self.status = "就绪".into();
        self.profiles.rename = false;
        self.profiles.add = self.accounts.players.is_empty();
        self.profiles.client = match accounts::client_id(&self.root) {
            Ok(client) => client,
            Err(e) => {
                self.message(Err(e), cx);
                None
            }
        };
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
    fn signed_in(&mut self, result: Result<Accounts>, cx: &mut Context<Self>) {
        let ready = result
            .as_ref()
            .is_ok_and(|accounts| accounts.current().is_some());
        self.account_result(result, cx);
        if ready
            && self.page == Page::Accounts
            && let Some(id) = self.launch_after_login.take()
            && let Some(index) = self.instances.iter().position(|i| i.id == id)
        {
            self.show_instance(index, cx);
            self.play(false, cx);
        }
    }
    fn cancel_microsoft(&mut self, cx: &mut Context<Self>) {
        if let Some(attempt) = self.login_cancel.take() {
            attempt.store(true, Ordering::Release);
            self.profiles.device = None;
            self.logging_in = false;
            self.status = "已取消微软登录".into();
            self.failed = false;
            cx.notify();
        }
    }

    fn open_verification(&mut self, cx: &mut Context<Self>) {
        if let Some(prompt) = &self.profiles.device
            && open::that_detached(&prompt.verification_uri).is_err()
        {
            self.status = "请在浏览器中打开弹窗内的微软验证地址".into();
        }
        cx.notify();
    }

    fn start_microsoft_login(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.profiles.client.clone() else {
            return;
        };
        let attempt = Arc::new(AtomicBool::new(false));
        self.login_cancel = Some(attempt.clone());
        self.logging_in = true;
        self.failed = false;
        self.status = "正在向微软获取登录代码…".into();
        let (tx, rx) = async_channel::bounded(1);
        let stop = attempt.clone();
        let task = cx.background_executor().spawn(async move {
            accounts::login(client, &stop, move |prompt| {
                tx.try_send(prompt)
                    .map_err(|_| anyhow::anyhow!("登录窗口已关闭"))
            })
        });
        cx.spawn(async move |view, cx| {
            if let Ok(prompt) = rx.recv().await {
                let _ = view.update(cx, |home, cx| {
                    if attempt.load(Ordering::Acquire) {
                        return;
                    }
                    cx.write_to_clipboard(ClipboardItem::new_string(prompt.user_code.clone()));
                    home.profiles.device = Some(prompt);
                    home.status = "登录代码已复制，等待微软授权".into();
                    home.open_verification(cx);
                });
            }
            let result = task.await;
            let _ = view.update(cx, |home, cx| {
                if attempt.load(Ordering::Acquire) {
                    return;
                }
                home.logging_in = false;
                home.login_cancel = None;
                home.profiles.device = None;
                let accounts = result.and_then(|login| login.save(&home.root));
                home.signed_in(accounts, cx);
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn device_dialog(&self, cx: &Context<Self>) -> Option<Dialog> {
        let prompt = self.profiles.device.as_ref()?;
        let cancel = cx.weak_entity();
        let reopen = cx.weak_entity();
        let copy = cx.weak_entity();
        Some(
            Dialog::new(
                "microsoft-device-dialog",
                "登录 Minecraft",
                move |_, cx| {
                    let _ = cancel.update(cx, |home, cx| home.cancel_microsoft(cx));
                },
            )
            .detail("登录代码已自动复制。请在微软页面输入此代码，然后返回启动器。")
            .child(heading(&prompt.user_code))
            .child(kicker("也可以在另一台设备上打开"))
            .child(div().text_sm().child(prompt.verification_uri.clone()))
            .action(move |_| {
                Button::new("reopen-verification", "重新打开网页").on_click(move |_, _, cx| {
                    let _ = reopen.update(cx, |home, cx| home.open_verification(cx));
                })
            })
            .action(move |_| {
                Button::new("copy-device-code", "复制代码").on_click(move |_, _, cx| {
                    let _ = copy.update(cx, |home, cx| {
                        if let Some(prompt) = &home.profiles.device {
                            cx.write_to_clipboard(ClipboardItem::new_string(
                                prompt.user_code.clone(),
                            ));
                            home.status = "登录代码已复制".into();
                            cx.notify();
                        }
                    });
                })
            })
            .action(|close| {
                Button::new("cancel-device-code", "取消")
                    .on_click(move |_, window, cx| close(window, cx))
            }),
        )
    }

    fn authenticate(&mut self, cx: &mut Context<Self>) {
        if self.logging_in {
            return;
        }
        let name = self.profiles.name.read(cx).text().trim().to_owned();
        if self.profiles.kind == Kind::Offline {
            let r = Player::offline(&name).and_then(|p| accounts::add(&self.root, p, None));
            self.signed_in(r, cx);
            return;
        }
        if self.profiles.kind == Kind::Microsoft {
            self.start_microsoft_login(cx);
            return;
        }
        let root = self.root.clone();
        let provider = self.profiles.server.read(cx).text().to_owned();
        let user = self.profiles.username.read(cx).text().to_owned();
        let password = self.profiles.password.read(cx).text().to_owned();
        self.profiles
            .password
            .update(cx, |f, cx| f.set_text("", cx));
        self.logging_in = true;
        self.failed = false;
        self.status = "正在联系认证服务".into();
        let task = cx.background_executor().spawn(async move {
            crate::third_party::login(&root, &provider, &user, &password, &name)
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |h, cx| {
                h.logging_in = false;
                h.login_cancel = None;
                h.signed_in(result, cx);
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
                    form = form.child(kicker(if self.profiles.client.is_some() {
                        "使用一次性代码在微软官方页面授权，也可以用手机完成。"
                    } else {
                        "此版本尚未开通微软登录服务，暂时无法获取登录代码。"
                    }));
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
                            && self.profiles.client.is_none()),
                )
                .on_click(cx.listener(|h, _, _, cx| h.authenticate(cx))),
            );
            if self.login_cancel.is_some() && self.profiles.device.is_none() {
                form = form.child(
                    Button::new("cancel-request", "取消登录")
                        .on_click(cx.listener(|h, _, _, cx| h.cancel_microsoft(cx))),
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
                                    h.signed_in(r, cx);
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
