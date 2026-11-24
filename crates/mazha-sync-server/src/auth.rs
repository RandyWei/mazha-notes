//! 最小鉴权（D100，契约见 `docs/SYNC-SPEC.md` §7.4.2）。
//!
//! 只做「设备 token → 允许的工作区集合」映射，`Authorization: Bearer <token>` 由 HTTP 层
//! 解析后传入。不做用户/组织权限、不读系统状态；token 比对是普通相等比较（MVP 范围，
//! 生产部署应由反向代理/网关承担限流与更严格的凭证管理）。

use std::collections::{BTreeMap, BTreeSet};

use mazha_sync::WorkspaceId;

/// 鉴权失败原因：统一映射为协议错误码 `Unauthorized`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    /// 请求没有可解析的 Bearer 凭证。
    #[error("缺少 Bearer 凭证")]
    MissingToken,
    /// 凭证有效但未被授权访问该工作区。
    #[error("凭证无权访问该工作区")]
    Forbidden,
}

/// 一条启动配置：一个 token 允许访问的工作区集合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRule {
    /// Bearer token 原文（由部署配置注入）。
    pub token: String,
    /// 该 token 可访问的工作区列表。
    pub workspaces: Vec<WorkspaceId>,
}

/// 设备 token + 工作区授权表。
#[derive(Debug, Clone, Default)]
pub struct TokenAuth {
    /// token → 可访问工作区集合（集合避免重复项影响判断）。
    rules: BTreeMap<String, BTreeSet<WorkspaceId>>,
}

impl TokenAuth {
    /// 由配置规则构造授权表。
    #[must_use]
    pub fn new(rules: impl IntoIterator<Item = TokenRule>) -> Self {
        let mut table = BTreeMap::new();
        for rule in rules {
            table.insert(
                rule.token,
                rule.workspaces.into_iter().collect::<BTreeSet<_>>(),
            );
        }
        Self { rules: table }
    }

    /// 校验 token 是否可访问指定工作区。
    ///
    /// # Errors
    /// 空凭证返回 [`AuthError::MissingToken`]；无权限返回 [`AuthError::Forbidden`]。
    pub fn authorize(&self, token: Option<&str>, workspace: &WorkspaceId) -> Result<(), AuthError> {
        let token = token.ok_or(AuthError::MissingToken)?;
        self.rules
            .get(token)
            .is_some_and(|allowed| allowed.contains(workspace))
            .then_some(())
            .ok_or(AuthError::Forbidden)
    }

    /// 只读判断，不区分「无凭证」与「无权限」。
    #[must_use]
    pub fn is_authorized(&self, token: Option<&str>, workspace: &WorkspaceId) -> bool {
        self.authorize(token, workspace).is_ok()
    }
}

/// 解析 `Authorization: Bearer <token>`；大小写与前缀严格匹配，空 token 视为缺失。
#[must_use]
pub fn bearer_token(header: &str) -> Option<&str> {
    header
        .strip_prefix("Bearer ")
        .filter(|token| !token.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth() -> TokenAuth {
        TokenAuth::new([
            TokenRule {
                token: "token-a".to_owned(),
                workspaces: vec![
                    WorkspaceId::new("ws-1").unwrap(),
                    WorkspaceId::new("工作区-甲").unwrap(),
                ],
            },
            TokenRule {
                token: "token-b".to_owned(),
                workspaces: vec![WorkspaceId::new("ws-2").unwrap()],
            },
        ])
    }

    #[test]
    fn bearer_header_parsing_is_strict() {
        assert_eq!(bearer_token("Bearer token-a"), Some("token-a"));
        assert_eq!(bearer_token("Bearer "), None);
        assert_eq!(bearer_token("bearer token-a"), None);
        assert_eq!(bearer_token("Basic token-a"), None);
    }

    #[test]
    fn token_can_only_access_authorized_workspaces() {
        let auth = auth();
        let ws1 = WorkspaceId::new("ws-1").unwrap();
        let ws2 = WorkspaceId::new("ws-2").unwrap();
        let cjk = WorkspaceId::new("工作区-甲").unwrap();
        assert!(auth.is_authorized(Some("token-a"), &ws1));
        assert!(auth.is_authorized(Some("token-a"), &cjk));
        assert!(!auth.is_authorized(Some("token-a"), &ws2));
        assert!(auth.is_authorized(Some("token-b"), &ws2));
        assert_eq!(
            auth.authorize(None, &ws1).unwrap_err(),
            AuthError::MissingToken
        );
        assert_eq!(
            auth.authorize(Some("token-a"), &ws2).unwrap_err(),
            AuthError::Forbidden
        );
        assert_eq!(
            auth.authorize(Some("unknown"), &ws1).unwrap_err(),
            AuthError::Forbidden
        );
    }
}
