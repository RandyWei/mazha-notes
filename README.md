# 码札 (Mazha)

> 原生 WYSIWYG Markdown 编辑器 · Rust + GPUI · 本地文件 · 跨平台

**码札**（Mazha，"札"取书信、笔记之意）是一款所见即所得（Typora 式）的
Markdown 编辑器。纯 Rust 技术栈构建，文档以本地 `.md` 文件保存，
面向个人笔记/知识库、开发者文档、通用写作与团队协作文档。

## 文档

- [SPEC.md](./SPEC.md) —— **规格说明书，本项目的单一事实来源（先改 SPEC 再改代码）**

## 架构一览

```
platforms/{desktop,ios,android,harmony,web}   UI 层（各平台独立）
                    │
         mazha-ffi / mazha-wasm               桥接层
                    │
                mazha-core                     共享核心（纯 Rust，不依赖 GUI）
        ┌───────────┼───────────┬──────────────┐
     mazha-md   mazha-crdt  mazha-store   mazha-export / mazha-plugin
```

## 开发

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all --check
```

- Rust edition 2024，最新 stable（见 `rust-toolchain.toml`）。
- 桌面平台（GPUI/gpui-kit）在 `platforms/desktop`，独立于 workspace 构建。

## 许可证

代码以 **AGPL-3.0-or-later** 开源（见 SPEC §1 与第 14 节决策记录）。
