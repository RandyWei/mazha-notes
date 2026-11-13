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

## 音视频预览（0.1.85，D64，macOS）

独占一行的单行 HTML `<video>` / `<audio>` 会渲染为播放控件，首次点击时才创建 AVPlayer。视频支持播放/暂停与点击进度条跳转，首帧前显示 `poster`，没有封面时使用 16:9 占位；音频只显示一个固定 `50×50` 的播放/暂停按钮，不占满正文宽度。光标进入该行会切回原始 HTML 以便编辑。

```html
<video src="https://example.com/demo.mp4" poster="cover.jpg" title="演示"></video>
<audio src="audio/interview.m4a" title="访谈"></audio>
<video controls><source src="demo.mp4"></video>
```

媒体标签必须完整写在一行，地址可为 `http(s)`、协议相对 URL、绝对路径、相对当前 Markdown 文件的本地路径或 `file://`。缺少 `src`、未闭合、跨行的标签保持原文；与正文混排时只显示占位标签。播放格式与编码取决于 macOS AVFoundation，暂不支持 data/blob 媒体、音量/字幕/倍速/循环/全屏/画中画或拖拽进度。

HTML/EPUB 导出保留可播放的 `video/audio controls` 标签；DOCX 与 PDF 只输出 `[视频]` / `[音频]` 占位。

## macOS 原生应用菜单（0.1.84，D63）

菜单栏包含「码札 / 文件 / 编辑 / 视图 / 窗口」：应用级命令（关于、服务、隐藏、退出）、文件命令（新建/打开/保存/另存/导出/关闭/移动）、编辑命令（撤销/重做/剪切/复制/粘贴/全选/查找）、视图命令（命令面板、文件树/大纲、主题、字体/字号、重置外观）与窗口命令（最小化/缩放/全屏）。菜单项与快捷键均由 GPUI 注册，应用不在前台时 macOS 会禁用菜单项。

常用快捷键：

| 操作 | 快捷键 |
|------|--------|
| 新建 / 打开 / 保存 / 另存为 | `Cmd+T` / `Cmd+O` / `Cmd+S` / `Cmd+Shift+S` |
| 导出 PDF / 命令面板 / 查找替换 | `Cmd+E` / `Cmd+P` / `Cmd+F` |
| 关闭标签页 / 关闭全部 | `Cmd+W` / `Cmd+Shift+W` |
| 撤销 / 重做 | `Cmd+Z` / `Cmd+Shift+Z` |
| 剪切 / 复制 / 粘贴 / 全选 | `Cmd+X` / `Cmd+C` / `Cmd+V` / `Cmd+A` |
| 循环主题 | `Cmd+Shift+L` |
| 最小化 / 全屏 | `Cmd+M` / `Ctrl+Cmd+F` |
| 隐藏 / 隐藏其他 / 退出 | `Cmd+H` / `Cmd+Option+H` / `Cmd+Q` |

裸二进制（`cargo run`）的应用菜单标题显示可执行名 `mazha-desktop`；打包后的 `码札.app` 使用 `Info.plist` 中的显示名「码札」。当前未提供「帮助」菜单。

## 外观设置（0.1.83，D62）

主题、正文字体/等宽字体与字号可运行时调整，退出时自动保存：

- 主题：跟随系统 / 浅色 / 深色；
- 正文字体/等宽字体：预设列表循环（含主题默认）；
- 正文字号：12–28px；等宽字号：10–24px。

操作方式：

| 操作 | 快捷键 |
|------|--------|
| 正文字号 +/− | `Cmd+=` / `Cmd+-` |
| 等宽字号 +/− | `Cmd+Option+=` / `Cmd+Option+-` |
| 循环主题 | `Cmd+Shift+L` |
| 重置外观 | `Cmd+Option+0` |

也可用 `Cmd+P` 命令面板执行「主题」「正文字体」「等宽字体」「字号」相关命令。当前字号与主题显示在底部状态栏。

设置文件默认写到平台配置目录（macOS `~/Library/Application Support/Mazha/settings.json`，Linux `$XDG_CONFIG_HOME/mazha/settings.json`，Windows `%APPDATA%\Mazha\settings.json`）；设置 `MAZHA_SETTINGS_PATH=/path/to/settings.json` 可覆盖，便于测试或多套配置。

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
