# Noctiluca

Noctiluca 是用 Rust 和 Ely GPUI 编写的 Windows 原生 Minecraft Java 版启动器。双击 `Noctiluca.exe`，从侧栏添加账号，选择实例并点击「启动游戏」。要使用已有整合包，可直接点侧栏「导入整合包」，选择本地文件。首次安装将准备 Java、游戏、资源和清单中指定的加载器。

## 实例与文件

- 「下载中心 → 游戏下载」依次提供 NeoForge、Forge、Fabric。输入 Minecraft 版本后，可以浏览该版本可用的加载器构建，选择「安装到新实例」下载游戏、Java、资源和所选加载器。
- 「下载中心 → 整合包」支持按加载器、游戏版本、名称和来源查找整合包。选择发布版本后，启动器按包清单安装到独立实例。也可以导入本地 Modrinth 的 `.mrpack` 或 CurseForge 的 `.zip`。如果基础游戏准备中断，选中已创建的实例，点击「安装 / 修复」继续。
- Modrinth 可以直接使用。CurseForge 的在线搜索和有文件依赖的清单需要环境变量 `NOCTILUCA_CURSEFORGE_API_KEY`。文件须经作者授权第三方下载，目前支持模组、资源包和光影三种 CurseForge 文件类型。[CurseForge API](https://docs.curseforge.com/rest-api/)
- 「实例管理」按加载器分组，可按名称、游戏版本或加载器搜索。点「添加已有目录」，选择 .minecraft、包含它的文件夹，或 versions 下的单个版本目录，扫描后在版本旁点「启动游戏」。游戏使用原目录的版本、运行库、资源、模组和存档。支持共用游戏目录与版本隔离目录。旧版已经复制过的实例仍使用各自的存档。
- 一个实例启动后可以回到侧栏启动另一个。各实例有独立的进程、日志、存档、模组目录和本地 DLL 目录。运行状态与进程号显示在「实例管理」。同一实例持有独占锁，公共游戏资源的安装依次进行。

- 「新建实例」支持按版本号搜索 Mojang 的正式版与快照。列表从 1.17 发布日起算。可选择原版、Fabric、Quilt、Forge 或 NeoForge。具体版本须得到对应加载器支持。
- 每个实例分别保存存档、设置、资源包与模组。可以为相同的游戏版本建立多个实例，设定不同的 Java、内存、JVM 参数和窗口分辨率。
- 「安装 / 修复」可单独执行下载与校验。已完成的文件会在重试时复用。「启动设置」中的共享资源来源可以指向已有的 `.minecraft`，安装器按清单和 SHA-1 验证后复制匹配的文件。
- 「备份与克隆」可以保存当前实例、复制游戏目录，或从本地备份恢复为一个新实例。备份包括世界、模组与配置。大型实例需要相应的额外磁盘空间。
- 「管理模组」按加载器和游戏版本搜索 Modrinth，安装模组及必需依赖，并可停用或重新启用本地 JAR。「导入整合包」按整合包指定的加载器版本导入必需客户端文件、通用配置和客户端配置。可选文件默认跳过。
- 「实时控制台」每秒更新当前日志的末尾，最多展示 100 行、32 KiB。可切换游戏、启动器与加载器日志，打开原始文件，或复制已遮盖常见令牌的片段。
- 可以给实例建立 Windows 快捷方式。例如目标参数 `--launch fabric-1.20.1` 会直接启动内置 Fabric 实例。自建实例的 ID 在其 `instance.json` 中。

在实例详情中，「游戏内存」支持跟随全局、自动和自定义。自定义模式分别调整初始内存（-Xms）与最大内存（-Xmx），下方显示当前系统用量与游戏分配。

点击「删除实例」并确认后，整个实例会移入已删除实例区。打开「实例管理 → 已删除实例」可恢复世界、模组与配置。

默认采用便携布局。程序与数据请放在可写目录中。

```text
Noctiluca.exe
Noctiluca-data/
  instances/<实例 ID>/instance.json
  instances/<实例 ID>/game/
  instances/<实例 ID>/launcher.log
  instances/<实例 ID>/last-launch.log
  shared/                              游戏文件、Java、运行库和公共资源
  removed-instances/<实例 ID>/         已移除的完整实例
  backups/<实例 ID>/                   本地备份
```

新安装使用 `Noctiluca-data`。升级时，程序旁只有 `MCSTART-data` 就继续使用原数据。`NOCTILUCA_HOME` 可指定数据目录，`NOCTILUCA_JAVA` 可指定默认 Java，`NOCTILUCA_CACHE_SOURCE` 可指定既有游戏缓存。旧版对应的 `MCSTART_` 环境变量仍可使用。

## 游戏账号

侧栏显示当前角色的头像、游戏名和登录类型。可添加多个账号，切换角色并管理头像和角色名。

- **离线** 输入角色名即可添加。首次安装游戏需要下载资源。服务器是否接受该身份取决于服务器的认证设置。选取本地皮肤 PNG 可以设置启动器里的头像。
- **Microsoft** 点击「微软正版登录」。Noctiluca 会显示并复制一次性代码，自动打开微软验证页。也可以在手机上打开弹窗里的网址完成授权。成功后自动显示 Java 角色，刷新令牌存放在 Windows 凭据管理器。
- **第三方** 填写认证站提供的 Yggdrasil HTTPS API 根地址和该站账号。多角色账号可指定角色名。游戏使用与该站配套的 authlib-injector 登录。[authlib-injector 项目](https://github.com/yushijinhun/authlib-injector)

启动器默认使用 Noctiluca Launcher 的公开客户端 ID。使用自有应用构建时，可通过 `NOCTILUCA_CLIENT_ID` 覆盖。旧版保存的服务配置继续生效。应用需支持个人微软账号、启用公共客户端流，并具备 Minecraft 服务访问权限。[微软设备代码协议](https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-device-code)

## 从源码构建

使用 Windows x64、Rust MSVC 工具链、Visual Studio C++ 构建工具、Windows SDK（含 `fxc.exe`）与 CMake。推荐在 Visual Studio 开发者终端中运行。依赖首次下载需要能访问 GitHub 和 crates.io。

```powershell
cargo run --locked
cargo test --locked
cargo build --release --locked
```

发布版生成在 `target/release/noctiluca.exe`。图标、字体与发布版 DirectX 着色器会随程序一起编译。项目固定了 Ely 和配套 GPUI 的版本。[Ely GPUI](https://github.com/ZacharyZhang-NY/Ely-GPUI-Components) · [Minecraft 启动内核](https://docs.rs/mc-launcher-core/0.1.2/mc_launcher_core/)

## 与成熟启动器对标的后续工作

| 领域 | 后续范围 |
| --- | --- |
| 游戏兼容 | 1.17 以前的资源布局与参数、旧版加载器、OptiFine、更多游戏与加载器组合的自动化验收 |
| 账号 | 正式发布应用的授权验收、已授权微软账号的离线策略 |
| 实例 | 分组、已有实例迁移向导、定时备份 |
| 内容与整合包 | 模组升级与冲突处理、整合包导出、更新与回滚、光影与世界在线浏览 |
| 下载 | 可选镜像、可配置代理、限速、单个大文件的断点续传 |
| 桌面 | 服务器收藏与直连、主题与多语言、自更新、macOS 和 Linux |

功能对标以 Prism Launcher、HMCL、PCL 的具体使用场景为准。
