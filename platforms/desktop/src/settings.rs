//! 桌面外观设置：主题模式、正文字体/字号、等宽字体/字号。
//!
//! 设置保存在系统配置目录下的 `Mazha/settings.json`，每次修改即时落盘。
//! 命令面板与快捷键只改本结构，渲染层再据此覆盖 gpui-kit 主题与编辑区字体。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 正文字号范围（px）。
pub const MIN_FONT_SIZE: f32 = 12.0;
pub const MAX_FONT_SIZE: f32 = 28.0;
/// 等宽字号范围（px）。
pub const MIN_MONO_FONT_SIZE: f32 = 10.0;
pub const MAX_MONO_FONT_SIZE: f32 = 24.0;
/// 每次调整的步长（px）。
pub const FONT_STEP: f32 = 1.0;

/// 主题偏好：跟随系统 / 固定浅色 / 固定深色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    /// 中文显示名，用于命令面板与状态栏。
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "跟随系统",
            Self::Light => "浅色",
            Self::Dark => "深色",
        }
    }

    /// 按「跟随系统 → 浅色 → 深色 → 跟随系统」循环。
    pub fn next(self) -> Self {
        match self {
            Self::System => Self::Light,
            Self::Light => Self::Dark,
            Self::Dark => Self::System,
        }
    }
}

/// 正文可选字体（空字符串 = 使用主题默认字体）。
const UI_FONT_CHOICES: &[(&str, &str)] = &[
    ("", "主题默认"),
    ("PingFang SC", "苹方"),
    ("Songti SC", "宋体"),
    ("Hiragino Sans GB", "冬青黑体"),
    ("Helvetica Neue", "Helvetica Neue"),
    ("Arial", "Arial"),
];

/// 等宽可选字体（空字符串 = 使用主题默认等宽字体）。
const MONO_FONT_CHOICES: &[(&str, &str)] = &[
    ("", "主题默认"),
    ("SF Mono", "SF Mono"),
    ("Menlo", "Menlo"),
    ("Monaco", "Monaco"),
    ("JetBrains Mono", "JetBrains Mono"),
    ("Fira Code", "Fira Code"),
    ("Cascadia Code", "Cascadia Code"),
];

/// 持久化的外观与视图设置；字体/字号为 `None` 时沿用主题默认值。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemePreference,
    /// 正文字体族；`None` = 主题默认。
    pub font_family: Option<String>,
    /// 等宽字体族；`None` = 主题默认。
    pub mono_font_family: Option<String>,
    /// 正文字号（px）；`None` = 主题默认。
    pub font_size: Option<f32>,
    /// 等宽字号（px）；`None` = 主题默认。
    pub mono_font_size: Option<f32>,
    /// 专注模式：淡化非当前块（D68）。
    pub focus_mode: bool,
    /// 打字机模式：保持光标行居中滚动（D68）。
    pub typewriter_mode: bool,
    /// 源码模式（代码模式）：按原始 Markdown 源码编辑/显示（D68）。
    pub source_mode: bool,
    /// 文件树展开状态：键为工作区根目录字符串，值为已展开的相对目录路径列表（D72）。
    pub tree_expanded: BTreeMap<String, Vec<String>>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemePreference::System,
            font_family: None,
            mono_font_family: None,
            font_size: None,
            mono_font_size: None,
            focus_mode: false,
            typewriter_mode: false,
            source_mode: false,
            tree_expanded: BTreeMap::new(),
        }
    }
}

impl Settings {
    /// 读取设置；文件缺失/损坏时回退默认值并做范围校正。
    pub fn load() -> Self {
        let mut settings = settings_path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
            .unwrap_or_default();
        settings.normalize();
        settings
    }

    /// 写入设置文件；失败时返回 IO/序列化错误，由调用方决定是否提示。
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = settings_path() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "找不到可写入的设置目录",
            ));
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, bytes)
    }

    /// 校正手工编辑配置可能带来的越界值/空白字体名。
    pub fn normalize(&mut self) {
        self.font_family = normalize_font(self.font_family.take());
        self.mono_font_family = normalize_font(self.mono_font_family.take());
        self.font_size = self.font_size.map(clamp_font_size);
        self.mono_font_size = self.mono_font_size.map(clamp_mono_font_size);
    }

    /// 正文字体显示名（用于命令面板/状态栏）。
    pub fn ui_font_label(&self) -> &'static str {
        font_label(UI_FONT_CHOICES, self.font_family.as_deref())
    }

    /// 等宽字体显示名。
    pub fn mono_font_label(&self) -> &'static str {
        font_label(MONO_FONT_CHOICES, self.mono_font_family.as_deref())
    }

    /// 循环切换正文字体；未知字体名从列表首项重新开始。
    pub fn cycle_ui_font(&mut self) {
        self.font_family = next_font(UI_FONT_CHOICES, self.font_family.as_deref());
    }

    /// 循环切换等宽字体。
    pub fn cycle_mono_font(&mut self) {
        self.mono_font_family = next_font(MONO_FONT_CHOICES, self.mono_font_family.as_deref());
    }

    /// 恢复默认外观（跟随系统 + 主题默认字体/字号），保留视图模式开关与文件树展开状态。
    pub fn reset_appearance(&mut self) {
        let focus_mode = self.focus_mode;
        let typewriter_mode = self.typewriter_mode;
        let source_mode = self.source_mode;
        let tree_expanded = std::mem::take(&mut self.tree_expanded);
        *self = Self::default();
        self.focus_mode = focus_mode;
        self.typewriter_mode = typewriter_mode;
        self.source_mode = source_mode;
        self.tree_expanded = tree_expanded;
    }

    /// 取某工作区已保存的文件树展开目录列表（无记录时为空）。
    pub fn tree_expanded_for(&self, root: &str) -> Vec<String> {
        self.tree_expanded.get(root).cloned().unwrap_or_default()
    }

    /// 记录某工作区的文件树展开目录列表；列表为空时移除该键，避免配置无限膨胀。
    pub fn set_tree_expanded_for(&mut self, root: &str, dirs: Vec<String>) {
        if dirs.is_empty() {
            self.tree_expanded.remove(root);
        } else {
            self.tree_expanded.insert(root.to_string(), dirs);
        }
    }
}

/// 把正文字号夹到允许范围并取整，避免手工配置出现半像素字号。
pub fn clamp_font_size(size: f32) -> f32 {
    size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE).round()
}

/// 把等宽字号夹到允许范围并取整。
pub fn clamp_mono_font_size(size: f32) -> f32 {
    size.clamp(MIN_MONO_FONT_SIZE, MAX_MONO_FONT_SIZE).round()
}

fn normalize_font(font: Option<String>) -> Option<String> {
    font.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn font_label(
    choices: &'static [(&'static str, &'static str)],
    current: Option<&str>,
) -> &'static str {
    let current = current.unwrap_or("");
    choices
        .iter()
        .find(|(value, _)| *value == current)
        .map(|(_, label)| *label)
        .unwrap_or("自定义")
}

fn next_font(
    choices: &'static [(&'static str, &'static str)],
    current: Option<&str>,
) -> Option<String> {
    let current = current.unwrap_or("");
    let index = choices
        .iter()
        .position(|(value, _)| *value == current)
        .unwrap_or(0);
    let (value, _) = choices[(index + 1) % choices.len()];
    (!value.is_empty()).then(|| value.to_string())
}

/// 设置文件路径：优先 `MAZHA_SETTINGS_PATH`，否则按平台惯例放到用户配置目录。
pub fn settings_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MAZHA_SETTINGS_PATH") {
        return Some(PathBuf::from(path));
    }
    #[cfg(target_os = "macos")]
    {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        Some(
            home.join("Library/Application Support/Mazha")
                .join("settings.json"),
        )
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = PathBuf::from(std::env::var_os("APPDATA")?);
        Some(appdata.join("Mazha").join("settings.json"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("mazha").join("settings.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_system_and_theme_fonts() {
        let settings = Settings::default();
        assert_eq!(settings.theme, ThemePreference::System);
        assert_eq!(settings.font_family, None);
        assert_eq!(settings.mono_font_family, None);
        assert_eq!(settings.ui_font_label(), "主题默认");
        assert_eq!(settings.mono_font_label(), "主题默认");
    }

    #[test]
    fn theme_preference_cycles_back_to_system() {
        assert_eq!(ThemePreference::System.next(), ThemePreference::Light);
        assert_eq!(ThemePreference::Light.next(), ThemePreference::Dark);
        assert_eq!(ThemePreference::Dark.next(), ThemePreference::System);
    }

    #[test]
    fn font_cycles_wrap_and_clear_to_theme_default() {
        let mut settings = Settings::default();
        settings.cycle_ui_font();
        assert_eq!(settings.font_family.as_deref(), Some("PingFang SC"));
        for _ in 0..UI_FONT_CHOICES.len() - 1 {
            settings.cycle_ui_font();
        }
        assert_eq!(settings.font_family, None);

        settings.cycle_mono_font();
        assert_eq!(settings.mono_font_family.as_deref(), Some("SF Mono"));
        for _ in 0..MONO_FONT_CHOICES.len() - 1 {
            settings.cycle_mono_font();
        }
        assert_eq!(settings.mono_font_family, None);
    }

    #[test]
    fn normalize_trims_fonts_and_clamps_sizes() {
        let mut settings = Settings {
            font_family: Some("  PingFang SC  ".into()),
            mono_font_family: Some("   ".into()),
            font_size: Some(99.0),
            mono_font_size: Some(1.0),
            ..Settings::default()
        };
        settings.normalize();
        assert_eq!(settings.font_family.as_deref(), Some("PingFang SC"));
        assert_eq!(settings.mono_font_family, None);
        assert_eq!(settings.font_size, Some(MAX_FONT_SIZE));
        assert_eq!(settings.mono_font_size, Some(MIN_MONO_FONT_SIZE));
    }

    #[test]
    fn settings_json_round_trip() {
        let settings = Settings {
            theme: ThemePreference::Dark,
            font_family: Some("Songti SC".into()),
            mono_font_family: Some("Menlo".into()),
            font_size: Some(18.0),
            mono_font_size: Some(14.0),
            focus_mode: true,
            typewriter_mode: true,
            source_mode: false,
            tree_expanded: BTreeMap::from([(
                "/tmp/ws".to_string(),
                vec!["sub".to_string(), "sub/deep".to_string()],
            )]),
        };
        let json = serde_json::to_string(&settings).unwrap();
        let parsed: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.theme, ThemePreference::Dark);
        assert_eq!(parsed.font_family.as_deref(), Some("Songti SC"));
        assert_eq!(parsed.mono_font_family.as_deref(), Some("Menlo"));
        assert_eq!(parsed.font_size, Some(18.0));
        assert_eq!(parsed.mono_font_size, Some(14.0));
        assert!(parsed.focus_mode);
        assert!(parsed.typewriter_mode);
        assert!(!parsed.source_mode);
        assert_eq!(
            parsed.tree_expanded_for("/tmp/ws"),
            vec!["sub".to_string(), "sub/deep".to_string()]
        );
    }

    /// 文件树展开状态按工作区根存取：空列表删除键，恢复默认外观不清空（D72）。
    #[test]
    fn tree_expanded_is_per_workspace_and_survives_reset() {
        let mut settings = Settings::default();
        assert!(settings.tree_expanded_for("/a").is_empty());
        settings.set_tree_expanded_for("/a", vec!["x".to_string()]);
        settings.set_tree_expanded_for("/b", vec!["y".to_string()]);
        assert_eq!(settings.tree_expanded_for("/a"), vec!["x".to_string()]);
        // 设为空列表时移除该工作区的键。
        settings.set_tree_expanded_for("/a", vec![]);
        assert!(settings.tree_expanded_for("/a").is_empty());
        assert_eq!(settings.tree_expanded_for("/b"), vec!["y".to_string()]);
        // 恢复默认外观保留展开状态与视图模式。
        settings.focus_mode = true;
        settings.reset_appearance();
        assert_eq!(settings.tree_expanded_for("/b"), vec!["y".to_string()]);
        assert!(settings.focus_mode);
    }

    #[test]
    fn reset_appearance_preserves_view_modes() {
        let mut settings = Settings {
            theme: ThemePreference::Dark,
            font_family: Some("Songti SC".into()),
            font_size: Some(20.0),
            focus_mode: true,
            typewriter_mode: true,
            source_mode: true,
            ..Settings::default()
        };
        settings.reset_appearance();
        assert_eq!(settings.theme, ThemePreference::System);
        assert_eq!(settings.font_family, None);
        assert_eq!(settings.font_size, None);
        assert!(settings.focus_mode);
        assert!(settings.typewriter_mode);
        assert!(settings.source_mode);
    }

    #[test]
    fn unknown_fonts_are_reported_as_custom() {
        let settings = Settings {
            font_family: Some("Some Font".into()),
            mono_font_family: Some("Other Mono".into()),
            ..Settings::default()
        };
        assert_eq!(settings.ui_font_label(), "自定义");
        assert_eq!(settings.mono_font_label(), "自定义");
    }
}
