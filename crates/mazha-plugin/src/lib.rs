//! # mazha-plugin
//!
//! WASM 插件宿主：加载插件、逐项能力授权（默认全部拒绝）、把
//! `storage-provider` 类插件桥接为 `mazha-store::StorageProvider`。
//! 见 `SPEC.md` §6.3。
//!
//! Phase 5 实现；运行时桌面用 `wasmtime`，移动端用 `wasmi`（无 JIT）。

/// 插件可申请的能力（逐项显式授权）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability {
    /// 读写指定目录范围内的文件。
    FileRead(String),
    /// 网络访问指定域名。
    Net(String),
    /// 访问剪贴板。
    Clipboard,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_is_comparable() {
        assert_eq!(Capability::Clipboard, Capability::Clipboard);
    }
}
