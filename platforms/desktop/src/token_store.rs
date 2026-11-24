//! 同步令牌的平台安全存储（Phase 5 S5b 第三批 / D109，契约见
//! `docs/SYNC-SPEC.md` §5.13/§9.8）。
//!
//! 读取优先级：**平台密钥链 → `MAZHA_SYNC_TOKEN` 环境变量**；密钥链为空而 env 有值时
//! 一次性迁移到密钥链，之后 env 只作调试/兼容回退。token 绝不写入 `settings.json`、
//! 日志、命令行参数或任何明文文件；本模块所有错误文案都不含 token 原文。
//!
//! 平台边界：macOS 直接调用 `security-framework` 的 generic password API（不引入
//! `keyring` 传递依赖）；Windows/Linux 当前用 `UnsupportedTokenStore` + env 回退，
//! 并在状态栏显示“平台密钥链不可用”诊断，不 panic。

use std::env;
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::sync::SYNC_TOKEN_ENV;

/// Keychain 服务名（只用于定位条目，不含敏感信息）。
const KEYCHAIN_SERVICE: &str = "com.mazha-notes.sync";
/// Keychain 账户名（只用于定位条目，不含敏感信息）。
const KEYCHAIN_ACCOUNT: &str = "sync-token";
/// token 最大长度：足够容纳常见 Bearer token，同时防止畸形输入灌满密钥链。
const MAX_TOKEN_BYTES: usize = 4096;
/// 非支持平台的统一诊断文案（状态栏直接展示）。
pub const UNSUPPORTED_DIAGNOSTIC: &str = "平台密钥链不可用（当前仅 macOS 支持），已回退环境变量";

/// 令牌存储错误：文案绝不携带 token 原文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenStoreError {
    /// 平台不支持密钥链（Windows/Linux 现阶段）。
    Unavailable(String),
    /// 后端读写失败（含系统密钥链拒绝访问）。
    Backend(String),
    /// token 空白/含控制字符/超长。
    InvalidToken(String),
    /// 密钥链与环境变量都没有可用 token。
    MissingToken,
}

impl fmt::Display for TokenStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(formatter, "平台密钥链不可用：{reason}"),
            Self::Backend(reason) => write!(formatter, "令牌存储访问失败：{reason}"),
            Self::InvalidToken(reason) => write!(formatter, "同步令牌无效：{reason}"),
            Self::MissingToken => write!(
                formatter,
                "缺少同步令牌（请存入系统密钥链或设置 {SYNC_TOKEN_ENV}）"
            ),
        }
    }
}

impl std::error::Error for TokenStoreError {}

/// token 来源：决定状态栏提示与迁移文案（不暴露 token）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    /// 来自平台密钥链。
    Keychain,
    /// 来自环境变量（密钥链不可用或写入失败时的回退）。
    Environment,
    /// 环境变量中的 token 已一次性迁移到密钥链。
    MigratedToKeychain,
}

impl TokenSource {
    /// 给用户看的简短来源说明。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Keychain => "系统密钥链",
            Self::Environment => "环境变量（调试回退）",
            Self::MigratedToKeychain => "系统密钥链（已从环境变量迁移）",
        }
    }
}

/// 解析结果：只有 token 与诊断文案，绝不记录来源值以外的任何敏感信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenResolution {
    /// 已校验、可直接用于 `Authorization: Bearer` 的 token。
    pub token: String,
    /// token 来源。
    pub source: TokenSource,
    /// 非阻断诊断（如平台密钥链不可用）；`None` 表示一切正常。
    pub diagnostic: Option<String>,
}

/// 可替换的 token 后端（同步阻塞、无 async）。
///
/// `Send + Sync` 让后端可随确认对话框的异步回调用 `Arc` 跨线程带回；所有实现
/// （内存假后端、Keychain 后端、不支持平台占位）都满足该约束。
pub trait TokenStore: Send + Sync {
    /// 读取 token；`None` 表示后端中没有条目（不是错误）。
    ///
    /// # Errors
    /// 平台不支持或后端访问失败时返回对应错误。
    fn get(&self) -> Result<Option<String>, TokenStoreError>;

    /// 写入/覆盖 token。
    ///
    /// # Errors
    /// token 非法、平台不支持或后端写入失败时返回对应错误。
    fn set(&self, token: &str) -> Result<(), TokenStoreError>;
}

/// 内存假实现：纯 Rust 单测用，绝不落盘。
#[derive(Debug, Default)]
pub struct InMemoryTokenStore {
    /// 当前 token（内存中的明文只在进程内存在）。
    value: Mutex<Option<String>>,
}

impl InMemoryTokenStore {
    /// 创建空存储。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 用给定 token 预置存储（测试用）。
    #[must_use]
    pub fn with_token(token: impl Into<String>) -> Self {
        Self {
            value: Mutex::new(Some(token.into())),
        }
    }
}

impl TokenStore for InMemoryTokenStore {
    fn get(&self) -> Result<Option<String>, TokenStoreError> {
        Ok(self.value.lock().unwrap_or_else(|p| p.into_inner()).clone())
    }

    fn set(&self, token: &str) -> Result<(), TokenStoreError> {
        let token = validate_token(token)?;
        *self.value.lock().unwrap_or_else(|p| p.into_inner()) = Some(token);
        Ok(())
    }
}

/// 校验 token：去首尾空白；空白/含控制字符/超长一律拒绝。
///
/// 返回值是规范化后的 token；不包含 token 的错误文案便于直接展示。
///
/// # Errors
/// 空串、任意空白/控制字符或超过 [`MAX_TOKEN_BYTES`] 时返回 [`TokenStoreError::InvalidToken`]。
pub fn validate_token(raw: &str) -> Result<String, TokenStoreError> {
    let token = raw.trim();
    if token.is_empty() {
        return Err(TokenStoreError::InvalidToken("token 不能为空".into()));
    }
    if token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(TokenStoreError::InvalidToken(
            "token 不能包含空白或控制字符".into(),
        ));
    }
    if token.len() > MAX_TOKEN_BYTES {
        return Err(TokenStoreError::InvalidToken(format!(
            "token 长度超过 {MAX_TOKEN_BYTES} 字节"
        )));
    }
    Ok(token.to_owned())
}

/// 按优先级解析 token：密钥链 → env（一次性迁移）→ `MissingToken`。
///
/// 密钥链读取失败（例如用户拒绝解锁）不直接失败：保留诊断并回退 env，
/// 这样“订阅/周期同步不会因密钥链故障被阻断”。
///
/// # Errors
/// 两处都没有 token 返回 [`TokenStoreError::MissingToken`]；
/// token 内容非法返回 [`TokenStoreError::InvalidToken`]。
pub fn resolve_token(store: &dyn TokenStore) -> Result<TokenResolution, TokenStoreError> {
    let mut diagnostic = None;
    match store.get() {
        Ok(Some(raw)) => {
            return Ok(TokenResolution {
                token: validate_token(&raw)?,
                source: TokenSource::Keychain,
                diagnostic: None,
            });
        }
        Ok(None) => {}
        Err(error) => {
            diagnostic = Some(error.to_string());
        }
    }
    let raw = env::var(SYNC_TOKEN_ENV).map_err(|_| TokenStoreError::MissingToken)?;
    let token = validate_token(&raw)?;
    match store.set(&token) {
        Ok(()) => Ok(TokenResolution {
            token,
            source: TokenSource::MigratedToKeychain,
            // 迁移成功时清掉“读取失败”诊断，避免误导用户以为仍在回退。
            diagnostic: None,
        }),
        Err(error) => {
            let reason = error.to_string();
            let merged = match diagnostic {
                Some(previous) if previous != reason => format!("{previous}；{reason}"),
                Some(previous) => previous,
                None => reason,
            };
            Ok(TokenResolution {
                token,
                source: TokenSource::Environment,
                diagnostic: Some(merged),
            })
        }
    }
}

/// 把存储错误转换为状态栏可展示的中文原因（不暴露 token）。
#[must_use]
pub fn token_error_message(error: &TokenStoreError) -> String {
    error.to_string()
}

/// 当前平台的默认 token 后端（macOS 用 Keychain，其余平台用不支持后端 + env 回退）。
#[must_use]
pub fn platform_store() -> Arc<dyn TokenStore> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(KeychainTokenStore)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(UnsupportedTokenStore)
    }
}

/// macOS 系统密钥链后端：直接调用 `security-framework`，不写任何明文文件。
#[cfg(target_os = "macos")]
#[derive(Debug, Default)]
pub struct KeychainTokenStore;

#[cfg(target_os = "macos")]
impl TokenStore for KeychainTokenStore {
    fn get(&self) -> Result<Option<String>, TokenStoreError> {
        // errSecItemNotFound 表示“没存过”，与真正的钥匙串故障区分开。
        const ERR_ITEM_NOT_FOUND: i32 = -25300;
        match security_framework::passwords::get_generic_password(
            KEYCHAIN_SERVICE,
            KEYCHAIN_ACCOUNT,
        ) {
            Ok(bytes) => {
                let raw = String::from_utf8(bytes).map_err(|_| {
                    TokenStoreError::Backend("密钥链中的 token 不是合法 UTF-8".into())
                })?;
                Ok(Some(raw))
            }
            Err(error) if error.code() == ERR_ITEM_NOT_FOUND => Ok(None),
            Err(error) => Err(TokenStoreError::Backend(error.to_string())),
        }
    }

    fn set(&self, token: &str) -> Result<(), TokenStoreError> {
        let token = validate_token(token)?;
        security_framework::passwords::set_generic_password(
            KEYCHAIN_SERVICE,
            KEYCHAIN_ACCOUNT,
            token.as_bytes(),
        )
        .map_err(|error| TokenStoreError::Backend(error.to_string()))
    }
}

/// 非 macOS 平台占位后端：只提供诊断，读写一律不可用，调用方回退 env。
#[cfg(not(target_os = "macos"))]
#[derive(Debug, Default)]
pub struct UnsupportedTokenStore;

#[cfg(not(target_os = "macos"))]
impl TokenStore for UnsupportedTokenStore {
    fn get(&self) -> Result<Option<String>, TokenStoreError> {
        Err(TokenStoreError::Unavailable(UNSUPPORTED_DIAGNOSTIC.into()))
    }

    fn set(&self, _token: &str) -> Result<(), TokenStoreError> {
        Err(TokenStoreError::Unavailable(UNSUPPORTED_DIAGNOSTIC.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 环境变量是进程全局状态：测试串行执行，且每个用例结束都恢复原值。
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// 在持有锁的前提下设置 env，并确保作用域结束恢复（包括 panic 前）。
    struct EnvGuard {
        previous: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn set(value: Option<&str>) -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let previous = env::var(SYNC_TOKEN_ENV).ok();
            match value {
                // SAFETY: 本模块测试串行持有 ENV_LOCK，且不启动其它线程读取该变量。
                Some(value) => unsafe { env::set_var(SYNC_TOKEN_ENV, value) },
                None => unsafe { env::remove_var(SYNC_TOKEN_ENV) },
            }
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                // SAFETY: 同上，恢复发生在同一把锁的作用域内，测试外无人并发读取。
                Some(value) => unsafe { env::set_var(SYNC_TOKEN_ENV, value) },
                None => unsafe { env::remove_var(SYNC_TOKEN_ENV) },
            }
        }
    }

    #[test]
    fn validate_token_rejects_blank_control_and_long() {
        assert_eq!(validate_token("  abc.def  ").unwrap(), "abc.def");
        for bad in [
            "",
            "   ",
            "a b",
            "a\nb",
            "a\u{0007}",
            &"x".repeat(MAX_TOKEN_BYTES + 1),
        ] {
            assert!(validate_token(bad).is_err(), "bad token 必须被拒绝");
        }
    }

    #[test]
    fn keychain_takes_priority_over_env() {
        let _guard = EnvGuard::set(Some("env-token"));
        let store = InMemoryTokenStore::with_token("keychain-token");
        let resolved = resolve_token(&store).unwrap();
        assert_eq!(resolved.token, "keychain-token");
        assert_eq!(resolved.source, TokenSource::Keychain);
        assert_eq!(resolved.diagnostic, None);
    }

    #[test]
    fn env_token_is_migrated_once_and_later_env_change_does_not_override() {
        let _guard = EnvGuard::set(Some("first-env"));
        let store = InMemoryTokenStore::new();
        let first = resolve_token(&store).unwrap();
        assert_eq!(first.source, TokenSource::MigratedToKeychain);
        assert_eq!(first.token, "first-env");

        // 迁移完成后改 env：密钥链仍优先，绝不被 env 覆盖。
        // SAFETY: 当前用例仍持有 ENV_LOCK，后面的 resolve_token 都是同线程串行读取。
        unsafe { env::set_var(SYNC_TOKEN_ENV, "second-env") };
        let second = resolve_token(&store).unwrap();
        assert_eq!(second.source, TokenSource::Keychain);
        assert_eq!(second.token, "first-env");
    }

    #[test]
    fn env_fallback_keeps_diagnostic_when_store_unavailable() {
        let _guard = EnvGuard::set(Some("env-only"));
        #[cfg(not(target_os = "macos"))]
        let store = UnsupportedTokenStore;
        #[cfg(target_os = "macos")]
        let store = FailingStore;

        let resolved = resolve_token(&store).unwrap();
        assert_eq!(resolved.source, TokenSource::Environment);
        assert_eq!(resolved.token, "env-only");
        assert!(
            resolved
                .diagnostic
                .as_deref()
                .is_some_and(|text| text.contains("平台密钥链不可用") || text.contains("失败"))
        );
    }

    #[test]
    fn missing_token_and_bad_env_are_reported_without_panic() {
        let _guard = EnvGuard::set(None);
        let store = InMemoryTokenStore::new();
        assert_eq!(
            resolve_token(&store).unwrap_err(),
            TokenStoreError::MissingToken
        );
        // SAFETY: 同上，坏 token 用例也始终在 ENV_LOCK 保护下修改进程环境。
        unsafe { env::set_var(SYNC_TOKEN_ENV, "bad token") };
        assert!(matches!(
            resolve_token(&store),
            Err(TokenStoreError::InvalidToken(_))
        ));
    }

    /// macOS 上模拟密钥链故障的假后端（只在对应测试里使用）。
    #[cfg(target_os = "macos")]
    #[derive(Debug)]
    struct FailingStore;

    #[cfg(target_os = "macos")]
    impl TokenStore for FailingStore {
        fn get(&self) -> Result<Option<String>, TokenStoreError> {
            Err(TokenStoreError::Backend("测试注入的密钥链故障".into()))
        }

        fn set(&self, _token: &str) -> Result<(), TokenStoreError> {
            Err(TokenStoreError::Backend("测试注入的密钥链故障".into()))
        }
    }
}
