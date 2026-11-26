use super::{LazyLock, Regex, known_failure, redact};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    Find(String),
    Java,
    Memory,
    Repair,
    Mods,
}

#[derive(Clone, Debug)]
pub(crate) struct Issue {
    pub summary: String,
    pub log: &'static str,
    pub action: Option<Action>,
}
impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.summary)
    }
}
impl std::error::Error for Issue {}

impl Issue {
    fn dependency(requester: &str, dep: &str, version: &str, actual: &str) -> Self {
        let action = match dep.to_ascii_lowercase().as_str() {
            "java" => Action::Java,
            "minecraft" | "fabricloader" | "forge" | "neoforge" | "quilt_loader" => Action::Mods,
            _ => Action::Find(dep.into()),
        };
        Self {
            log: "last-launch.log",
            summary: format!("「{requester}」需要 {dep}（{version}）。{actual}"),
            action: Some(action),
        }
    }

    pub fn from_log(text: &str) -> Self {
        static FABRIC: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?i)\bmod '([^'\r\n]+)' \([^)]+\)[^\r\n]*? requires ([^\r\n]{1,160}?) of (?:mod )?'([^'\r\n]+)'(?: \(([\w.-]+)\))?, (which is missing|but [^\r\n]{1,180})").unwrap()
        });
        static FORGE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?i)Mod ID:\s*'([\w.-]+)',\s*Requested by:\s*'([\w.-]+)',\s*Expected range:\s*'([^']+)',\s*Actual version:\s*'([^']+)'").unwrap()
        });
        static REQUIRES: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?im)(?:Failure message:\s*)?\bMod\s+([\w.-]+)\s+requires\s+([\w.-]+)\s+([^\r\n]{1,140})\r?\n\s*(?:Currently,\s*)?([\w.-]+) (is not installed|is missing|is [^\r\n]{1,100})").unwrap()
        });
        static FORMATTING: LazyLock<Regex> =
            LazyLock::new(|| Regex::new("(?i)§[0-9a-fk-or]").unwrap());
        let plain = FORMATTING.replace_all(text, "");
        let text = plain.as_ref();
        if let Some(c) = FABRIC.captures(text) {
            return Self::dependency(
                &c[1],
                c.get(4).map_or(&c[3], |m| m.as_str()),
                &c[2],
                if c[5].starts_with("which") {
                    "当前实例缺少这个依赖。"
                } else {
                    "已安装的版本不符合要求。"
                },
            );
        }
        if let Some(c) = FORGE.captures(text) {
            let actual = if c[4].eq_ignore_ascii_case("[MISSING]") {
                "当前实例缺少这个依赖。".into()
            } else {
                format!("当前版本为 {}。", &c[4])
            };
            return Self::dependency(&c[2], &c[1], &c[3], &actual);
        }
        if let Some(c) = REQUIRES.captures(text)
            && c[2].eq_ignore_ascii_case(&c[4])
        {
            let actual = if c[5].contains("not installed") || c[5].contains("missing") {
                "当前实例缺少这个依赖。".to_owned()
            } else {
                format!("当前版本：{}。", c[5].trim_start_matches("is "))
            };
            return Self::dependency(&c[1], &c[2], c[3].trim(), &actual);
        }
        let (summary, action) = known_failure(text).unwrap_or((
            "日志里没有明确的失败原因。可以查看本次输出，或导出诊断包。",
            None,
        ));
        Self {
            log: "last-launch.log",
            summary: summary.into(),
            action,
        }
    }

    pub fn from_error(e: &anyhow::Error) -> Self {
        if let Some(issue) = e.downcast_ref::<Self>() {
            return issue.clone();
        }
        let detail = redact(&format!("{e:#}"));
        let mut issue = Self::from_log(&detail);
        if detail.contains("无法执行所选 Java")
            || detail.contains("至少需要 Java")
            || detail.contains("请设置 NOCTILUCA_JAVA")
        {
            return Self {
                log: "launcher.log",
                summary: detail.chars().take(650).collect(),
                action: Some(Action::Java),
            };
        }
        if issue.action.is_none() && known_failure(&detail).is_none() {
            issue.summary = detail.chars().take(650).collect();
        }
        if e.downcast_ref::<crate::download::FileFailure>().is_some() {
            issue.action = Some(Action::Repair);
        }
        issue.log = "launcher.log";
        issue
    }
}
