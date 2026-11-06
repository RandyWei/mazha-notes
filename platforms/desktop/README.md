# platforms/desktop — 桌面端点（macOS / Windows / Linux）

技术栈：**GPUI + gpui-kit**（见 `SPEC.md` §7.1）。macOS / Windows 为 MVP 平台。

**当前状态**：Phase 4 桌面 MVP 主体达成（完整能力见 `HANDOFF.md` 与 `SPEC.md` §15）。

- 独立于根 workspace 构建（GPUI 依赖很重，避免拖慢核心 crate 构建；根 `Cargo.toml` 已 `exclude = ["platforms"]`）。
- 依赖：`gpui-kit = "0.7"`（内部锁定匹配的 `gpui-pre` 快照，**不要再单独引入 `gpui`**，否则两份类型不兼容）+ `mazha-core`（路径依赖）。

## 运行

```bash
cd platforms/desktop
cargo run                    # 打开内置示例文档（覆盖主要 Markdown 语法）
cargo run -- path/to/note.md # 打开指定 .md
```

## 打包 macOS .app

```bash
cd platforms/desktop
./scripts/bundle-macos.sh          # release 构建 + ad-hoc 签名
./scripts/bundle-macos.sh --debug  # debug 构建（快速验证打包）
```

产物在 `build/macos/码札.app` 与 `build/macos/Mazha-<版本>-macos.zip`。脚本会：

- 从 `assets/appicon.png` 生成完整 `AppIcon.icns`；
- 写入 `macos/Info.plist`（Bundle ID `icu.bughub.app.mazha`、版本取 `SPEC.md` 最新变更记录）；
- 复制二进制、做 ad-hoc 签名并校验。

正式对外分发需设置 `MAZHA_CODESIGN_IDENTITY="Developer ID Application: ..."` 做 Developer ID 签名，再配置 notarytool 凭据并执行：

```bash
MAZHA_NOTARY_PROFILE=mazha-notary ./scripts/notarize-macos.sh
```

该脚本提交 Apple 公证、`stapler` 装订票据，并用 `spctl` 验证 Gatekeeper 评估。

## 已实现（Phase 2 第一步）

`.md` → `Document`/`Projection`（core，源文本真相 + 隐藏语法标记）→ GPUI 逐行只读绘制：

- 块级：标题字号/字重（1–6）、列表符号（有序 `1.` / 无序 `•`）、任务复选框、引用缩进 + 左竖线、代码块等宽 + 弱底色、分割线；
- 行内：粗体、斜体、删除线、行内代码、链接（下划线 + 主题链接色）、图片占位；
- 主题：颜色取自 gpui-kit 主题（跟随深浅色），等宽字体取 `Theme::mono_font_family`。

## 待办

- 自写 `Element` + `shape_line` 绘制（命中测试、光标）——见 SPEC §7.1；
- WYSIWYG 语法标记显隐（光标进入行才显示 `#`/`**`/`-` 等）；
- IME（`EntityInputHandler`）与编辑命令接入（`Document::apply`）。
