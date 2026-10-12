//! # mazha-wasm
//!
//! Web 绑定：把 `mazha-core`（及可选地 WYSIWYG 投影）暴露给浏览器。
//!
//! 技术路线在 Phase 7 前经 spike 后从二选一（`SPEC.md` §7.3）：
//! - 方案 A：GPUI 编译到 wasm（与桌面同一套 Rust UI）；
//! - 方案 B：TypeScript 前端 + `mazha-core` 经 wasm-bindgen。

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {}
}
