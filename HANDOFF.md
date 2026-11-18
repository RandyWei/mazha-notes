# HANDOFF — 码札 / Mazha 交接与进度总览

> 每次提交代码时**同步更新本文件**（约定，见 `AGENTS.md`）。
> 单一事实来源是 `SPEC.md`（接口契约 §6、决策 §14、变更记录 §15）；本文件只做「当前进度 / 下一步」的归总，不代替 SPEC。

- 最近更新：2026-11-18
- 当前阶段：**Phase 1（共享核心）已完成**；**Phase 2/3 已完成**（桌面只读投影 → WYSIWYG 标记显隐/命中 → IME/编辑命令/多光标 → 文档级坐标 API → 块级 widget 渲染 → 表格增删行列/代码高亮/软换行/剪贴板/视觉行移动/查找替换/公式与图表/脚注/图片本地/远程/data/协议相对渲染/内联 HTML 标签与颜色/HTML 音视频建模）；**Phase 4 桌面 MVP 主体达成**（文件管理打开/保存/新建/重命名/删除/移动 + **文件树层级化（目录展开折叠/目录内新建，D70）** + **文件树拖拽移动/新建文件夹（D71）** + **展开状态持久化（D72）** + **拖拽落点高亮/跨应用拖拽（D73）** + **入站外部拖拽（从 Finder 拖入，D74）** + **侧栏标题固定（D75）** + **数学公式真实排版 RaTeX（D76，块级公式居左/点击编辑/无边框/编辑态显 TeX，行内不重叠）** + **Mermaid 图表真实渲染（D77，R4 第二步，mermaid-rs → SVG → GPUI 栅格化）** + **全工作区搜索 Cmd+Shift+F（D40）** + 多标签页 D37 + 命令面板 D38 + 大纲/目录导航 D39（**已移至右侧独立面板 D70**）+ 文本选区 + 浅灰选中/标签关闭/底部状态栏 D59 + macOS Dock 图标 D60 + macOS `.app` 打包 D61 + 外观设置 D62 + macOS 原生应用菜单 D63 + AVPlayer 音视频预览 D64 + 视图模式 D68 + 脚注双向跳转与导出编号/文末归集 D69，源码模式已修复任务列表/引用块渲染装饰残留）；**导出线**并行：HTML / PDF / DOCX / EPUB 均可用（桌面菜单/命令面板四格式入口已于 0.1.108 补齐，D82；PDF 缺图降级 + 当前文档目录作资源根，内置 SAMPLE 不再引用不存在的资源）；0.1.109 新增**导出不阻塞窗口 + 底部状态栏反馈（D83）**与**渲染后公式/图表进入导出文件（D84：HTML/EPUB/PDF 内嵌 SVG，DOCX 仍字面）**
- 仓库：`git@github.com:RandyWei/mazha-notes.git`（私有，发版后转公开）；分支 `main`
- 当前版本：**0.1.109**（细节见 SPEC §15 变更记录；本节只列归总）

## 门禁（提交前必须全绿）

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# 桌面端（独立 workspace，见 R10；触碰 platforms/* 必须单独跑）
cd platforms/desktop && cargo fmt --check && cargo clippy -- -D warnings && cargo test && cargo build
```

> rustc 需 **≥ 1.92**（workspace `rust-version`，Typst 0.15 的 MSRV），工具链见 `rust-toolchain.toml`（stable）。

## crate 进度

> 详细变更逐版本记录在 `SPEC.md` §15；下表只给「能力面 + 待办」。

| crate | 状态 | 能力面（要点） | 待办 |
|-------|------|----------------|------|
| `mazha-core` | ✅ Phase 1 完成 | `Document`（rope/编辑/undo-redo/块缓存+投影缓存**双增量**维护/`apply_and_sync`）、`OffsetMap`、`Projection`（`BlockStyle`/`TextRun`/`WidgetKind::{Image,Media}`/`RunStyle` + **块级 widget** `BlockWidget{source_range,line_range,kind}`、`BlockWidgetKind::{Code{lang,fenced},Table{align},Rule,Math,Chart{lang},Image{alt,dest}}` + `block_widget_at`/`block_widget_on_line`）、**文档级跨行坐标**（`DocPos`/`LineDisplayRange`/`source_to_doc_pos`/`doc_pos_to_source`/`line_of_source`/`selection_display_ranges`）、显示列↔字节换算（软换行用）、`commands`、`IntentMapper`、`table::edit_table`、`highlight::Highlighter`（syntect）、`find::{find_all,replace_all,replace_all_edit}`、导航（行范围/CJK 词范围）、`Workspace`（open/save/create/remove/rename/list/find）、`crdt` 桥接 | `DocId` |
| `mazha-md` | ✅ Phase 1 完成（方言全齐） | `parse`（ATX/Setext 标题、段落、分割线、围栏/缩进代码、引用、列表/任务、GFM 表格、Callout、Front-matter、**`$$…$$` 显示公式**、**`[^label]: body` 脚注定义**）、`parse_inline`（强调/粗体/删除线/行内代码/链接/图片/自动链接/wiki-link/tags/`$…$` 行内公式/**`[^label]` 脚注引用**/转义/**内联 HTML 标签与 `<font>` 颜色/`<br>` 剥离**，D56；**单行 `<video>`/`<audio>` 媒体元素**，D64）、`serialize_source`/`to_md`（前缀穿线，逐字节往返 + 幂等）、`reparse`（脏块增量 + 裁切点安全校验）、`FootnoteIndex`（引用首次出现顺序编号 + 引用/定义区间查询，D69）；列表项续行只剥标记宽度缩进（D81 修正，嵌套脚注定义续行不再脱离定义块） | HTML 块、惰性续行、多段落脚注（见 SPEC §9.1/§15） |
| `mazha-store` | ✅ MVP | `StorageProvider`（同步，D24）、`FileSystemProvider`、`DocPath`、`Entry`、`StoreError` | 文件监听、异步/流式（Phase 5）、云端/插件 Provider |
| `mazha-crdt` | ✅ MVP | `CrdtDoc`（`Y.Text`，字符↔字节）、insert/delete/replace/push、状态向量 + 增量更新（Yjs v1）、`apply_update`、`fork`、`snapshot`、并发收敛、落盘（`save_crdt`/`load_crdt`） | 多文档名册 |
| `mazha-export` | ✅ HTML + PDF + DOCX + EPUB + 主题/模板 + 缺图降级（D82） | `to_html*`（完整外壳 + 默认 CSS + 主题）、`to_typst*` + `to_pdf*`（`ExportWorld` 实现 `typst::World` → `typst::compile` + `typst_pdf`；`PdfDocument{bytes,warnings,pages}`；真实文档回归 `tests/pdf_real_doc.rs`）、`to_docx*`（docx-rs 1.1 直出 OOXML）、`to_epub*`（Zip + XHTML，EPUB 3，输出确定）、`Theme`/`Template`；**脚注导出前规整**（`footnotes::prepare`：按引用顺序编号 + **所有**定义含嵌套归集文末并剥容器前缀，HTML/EPUB 带 `<sup class="fnref">` 唯一锚点 + `<section class="footnotes"><ol>` + 定义侧 `↩` 回链，Typst/DOCX 纯文本编号，D69/D81）；块/行内特性与编辑器同源（含公式、脚注；媒体在 HTML/EPUB 保留可播放标签，PDF/DOCX 输出占位）；**PDF 缺图降级**（`pdf_image_path`/`replace_unresolvable_images` 纯函数，D82：缺文件/远程/data/绝对路径图片在独立副本里替换为占位文字，不再阻断 Typst 编译；`PdfExportOptions::base_dir` 由桌面传当前文档目录解析真实图片）；**渲染资产桥接（D84）**：`collect_render_requests`/`prepare_renderables` 纯函数 + `RenderAssets`/`AssetTarget`，`export_with_assets`/`to_pdf_with_assets` 经 `ExportWorld` 内存虚拟文件把桌面渲染的 SVG 嵌入 PDF，HTML/EPUB 用 data URI，DOCX 维持字面 | 字体子集（Web，Phase 7） |
| `mazha-plugin` | ⬜ 占位 | — | WASM 宿主、能力授权（Phase 5） |
| `mazha-ffi` | ⬜ 占位 | — | UniFFI 绑定（Phase 6） |
| `mazha-wasm` | ⬜ 占位 | — | Web 绑定（Phase 7） |

## 平台进度（`platforms/*`，各自独立 workspace）

| 平台 | 状态 | 已实现（要点） | 待办 |
|------|------|----------------|------|
| `desktop`（GPUI + gpui-kit 0.7） | 🟡 Phase 2/3 主体完成（Phase 4 MVP 主体达成） | 只读投影渲染 + 主题；光标/点击命中 + **行内格式局部展开**（光标在链接/粗体/斜体/删除线/行内代码内时**该组**显示原文，`inline_reveals`/`reveal_inline`，D42）+ **Cmd+点击打开链接**；行内代码用正文前景色、引用块整行浅底（各层引用都铺底 + 左侧竖线，仅最外层加上下 padding；底色限制在正文列内，D52）；自写 `Element`（连续布局 + 视觉行表 `row_starts`/`row_tops`，软换行后逐视觉行建顶边）+ 跨行选区；**滚动**：自持 `ScrollHandle`（`track_scroll`）+ 外层叠加 `Scrollbar`，内容高度两帧收敛、光标行滚动跟随（含 `cx.notify`）；**IME**：全 UTF-16 区间换算、组合文本按 marked 区间替换（修复拼音字母逐个上屏）；空行/纯语法行命中落行首；编辑命令（行内 Cmd+B/I/X/`、块级 Cmd+1-6/段落/引用/列表）；多光标（Alt+Click）；**四格式导出**（HTML/PDF/DOCX/EPUB，菜单与命令面板入口；`Cmd+E` 仍 PDF，D82）+ PDF 缺图降级与当前文档目录作资源根；**导出不阻塞（D83：后台线程池编译/写文件 + 状态栏「正在导出/已导出/失败」+ 6s 按序号清除）**、**导出渲染公式/图表（D84：桌面 RaTeX/mermaid 产 SVG，HTML/EPUB 内嵌 data URI，PDF 虚拟文件矢量嵌入，DOCX 字面限制）**；文档级坐标 API 接入；块级 widget 装饰（代码面板**浅底+语言标签**、表格网格、分割线、**公式描边、图表弱底+标签**）；**中文斜体渲染器级合成倾斜**（macOS 中文字体无斜体字面、CoreText 也不为 CJK 合成，故 `oblique.rs` 取 CT 字形轮廓做 12° 剪切自绘，CJK 段以透明 run 占位保留排版；D43，D79 起轮廓按 `(字符, 字号)` 缓存且原点按字形 y 微调 + DPR 像素对齐校正；D80 起主路径改为 CoreText 直接栅格化 12° 剪切字形出 DPR 位图叠加，根治路径自绘发虚，D79 轮廓作回退）；**行内代码可见底色**（`code_bg` = 前景 alpha 0.08，经 `TextRun.background_color` 逐视觉行绘制）；**代码块浅底 + 左右 padding**（`code_block_bg`，缩进 +1.5em、文本 +12px、面板右缘与正文右边距对齐，D51）；**Tab/Shift+Tab 缩进/反缩进**（D44：覆盖 Root 的焦点切换绑定，行选区整体缩进，多光标重映射）；**列表/任务回车续行**（D45：走核心 `IntentMapper`，新项图标即时显示，空项再回车退出列表，嵌套保留缩进，引用内列表也续行；修复空 `ListItem` 无样式导致图标不显示）；**Typora 式就地显源码**（光标进入围栏代码块/独占图片行 → 整行就地显出原文，`BlockGeom::raw_src` 走 1:1 字符↔字节映射，移开复原）；**图片真实渲染**（D53/D54/D55：核心保留 `dest`，独占行产出 `BlockWidgetKind::Image`；桌面按文档目录解析本地路径、`http(s)` 与协议相对 `//host/...` 走 GPUI `Resource::Uri` + `ImageAssetLoader` 下载缓存，`data:image/...` 内联 base64/百分号编码走 `Image::from_bytes`，按正文宽等比缩放且不放大小图，行高随图片；光标进入显示 `![alt](dest)`，缺失/行内图片仍占位；启动时安装 `reqwest` HTTP 客户端）；表格增删行列 + **表格观感**（表头加粗/底色、分隔行收起、单元格留白，D46）+ **按内容列宽/竖向网格线/列对齐 + 单元格 Tab 导航**（D47）+ **表格内左右方向键跳过隐藏标记/竖线/填充**（D48）+ **表格光标视觉命中按最近格距离选择**（D49）+ **从表格外横移第一下即进入单元格**（D50）+ **超宽表格列宽压缩 + 单元格软换行**（D57）；代码高亮；软换行；剪贴板 Cmd+C/X/V；视觉行上下移动；查找/替换 Cmd+F；**数学公式真实排版**（D76：`ratex-gpui` render-only，`$$…$$` 块与 `$…$` 行内在渲染态栅格化为透明底公式图像并叠加显示（块级居左对齐正文文本起点、行内垂直居中；块级点图像任意处进源码编辑态、整块显含 `$$` 的 TeX 原文、无背景/边框；行内裁掉 RaTeX 透明留白后按占位槽宽绘制，不压右侧文字），TeX 文本转透明占位保留光标/选区/偏移映射，按 `(tex, 字号, DPR, 颜色)` 缓存，光标进入/源码模式显示原文，渲染失败回退字面 TeX）；**Mermaid 图表真实渲染**（D77，R4 第二步：`mermaid-rs-renderer 0.3.1` render-only 纯 Rust 产 SVG → GPUI `SvgRenderer` 栅格化，按 DSL 源码缓存含失败缓存；渲染态源码转透明占位、锚点行显示图像（透明背景、无弱底/描边，按正文宽等比适配）、块内其余行归零，`chart_boxes` 命中图像进 DSL 编辑态移开恢复；失败/不支持语法回退弱底 + 语言标签占位面板；核心/导出不改）；脚注标签 `RunStyle::Footnote`（主题色）+ **Cmd+点击双向跳转**（引用 ↔ 定义，D69）；**内联 HTML 标签**（`<font color>`/`<span style>` 自定义前景色经 `TextRun.color` 透传，`<b>/<i>/<s>/<code>` 映射既有样式，`<br>` 仅剥离）；**Phase 4：打开 Cmd+O / 保存 Cmd+S / 另存为 Cmd+Shift+S、**文件树层级化（D70：按 `Workspace::list` 递归渲染目录/文件，目录 `▸/▾` 展开折叠、目录行 `＋` 在目录内新建、`✎` 重命名、`✕` 递归删除，侧栏标题栏 `＋` 根目录新建；删除/重命名目录同步迁移展开态与当前文档路径；宽度可拖动 160–420px；长文件名末尾省略号；**拖拽移动 + 新建文件夹** D71：文件/目录行可拖拽，目录行与侧栏标题为落点（`can_drop`/`on_drop`），落点经 `move_entry` 移动并迁移展开态/当前文档路径，标题栏 `＋文件`/`＋文件夹`；**展开状态持久化** D72：按工作区根存入 `settings.json` 的 `tree_expanded`，启动恢复、切换/CRUD 后落盘；**拖拽落点高亮 + 跨应用拖拽** D73：目录行/侧栏标题 `drag_over` 铺强调底色（非法落点由 `can_drop` 阻止），文件/目录行 `external_drag_payload` 支持拖出窗口交给系统原生文件拖拽，已实机验收通过（落入目标目录时复制文件，原条目保留）；**全工作区搜索** D40：`Cmd+Shift+F` 居中悬浮面板即时递归扫描工作区 Markdown，按文件分组列出命中行（命中高亮、按字符截断），`↑↓` 选择、`Enter`/点击打开并选中、`Cmd+Alt+C` 大小写、上限 500 条标记截断、读取失败计入跳过，命令面板/编辑菜单同步入口）、脏状态 `*`；多标签页（D37/D58，Cmd+T/W/Shift+W/1..9/0；标签点击/侧栏打开统一装载，保留撤销历史）；命令面板（D38，Cmd+P，命令表+过滤+执行）；大纲/目录导航（D39，**已移至右侧独立可拖动面板** D70，标题列表+点击跳转，Cmd+Shift+O）；**UI 细化（D59）：活动标签/侧栏当前项浅灰选中（文本选区仍蓝色）、大纲跳转目标行置顶、标签 `✕` 关闭按钮（单标签隐藏）、文件信息/行数/快捷键移入底部 24px 全宽状态栏；macOS Dock 图标（D60，`assets/appicon.png` 编入二进制 → `NSApplication.setApplicationIconImage`）；**macOS `.app` 打包（D61，`scripts/bundle-macos.sh` 生成 `AppIcon.icns` + Info.plist + ad-hoc/Developer ID 签名 + zip）**；**外观设置（D62，主题/字体/字号持久化 + 命令面板/快捷键 + 状态栏）**；**视图模式（D68，专注模式淡化非当前块、打字机模式居中光标行、源码模式按原始 Markdown 源码编辑显示，可持久化并通过菜单/命令面板/快捷键切换）**；**全工作区搜索（D40，`Cmd+Shift+F` 悬浮面板 + 分组命中 + 打开定位）**；**文件树拖拽落点高亮/跨应用拖拽（D73）+ 入站外部拖拽（D74，从 Finder 拖入：复制导入目录、Markdown 打开、其它文件插入相对链接）+ **侧栏标题固定（D75：标题与＋文件/＋文件夹始终可见，仅条目列表滚动）** | 入站（Finder→窗口）、出站（拖出窗口→目标目录复制文件）均已实机验收通过 |
| `ios` / `android` / `harmony` / `web` | ⬜ 仅 README | — | Phase 6 / 8 / 7 |

> 桌面端入口：`cargo run -- [path/to/file.md]`。渲染效果可可视化验证：`swift` 查窗口号（CGWindowListCopyWindowInfo）→ `screencapture -x -o -l<窗口号>` 截图 → PIL 逐像素核对底色/字形（本机屏幕录制权限已可用）。交互可用 `CGEvent.postToPid` 合成按键（无需 `osascript` 置前；置前脚本偶发 -1719，重试即可），已验证 Tab/Shift+Tab/回车等链路。

## 测试现状

| crate | 数量 |
|-------|------|
| `mazha-core` | 204 |
| `mazha-md` | 177 |
| `mazha-export` | 155 单测 + 3 集成（`tests/pdf_real_doc.rs`） |
| `mazha-crdt` | 11 |
| 合计 | **565**（其余含 store 与占位 crate 测试） |
| `platforms/desktop`（独立 workspace） | 91（不计入上面的 565） |

## 关键决策索引（全文见 SPEC §14）

- **D1–D22**：重写、GPUI+gpui-kit、纯 Rust 核心、源文本真相、GFM+知识库方言、yrs CRDT、WASM 插件、AGPL-3.0 等。
- **D23**：Markdown **自研行扫描**（精确字节偏移）；`pulldown-cmark` 仅 dev 对照 → [`docs/ADR/ADR-001`](docs/ADR/ADR-001-self-authored-markdown-parser.md)。
- **D24**：`StorageProvider` 同步接口（本地文件为唯一 MVP Provider）。
- **D25**：PDF = `to_typst` + 自实现 `ExportWorld` → `typst::compile` + `typst_pdf`；MSRV 提到 1.92。
- **D26/D27/D28**：DOCX 走 docx-rs 直出 OOXML；EPUB 走 Zip + XHTML（EPUB 3）；导出外观拆 `Theme`（CSS）与 `Template`（Typst 前置）。
- **D29**：桌面端坐标/选区统一走核心文档级 API（并修复历史编译债 + 独立 `Cargo.lock`）。
- **D30**：块级 widget 建模在**核心投影**，核心只给「分组与范围」，绘制留 UI；"光标进入才可编辑"由 UI 判定。
- **D31**：表格增删行列是核心纯函数 `edit_table` → 规范化重写整块 → 单次 `Edit::Replace`。
- **D32**：代码高亮用 **syntect**（纯 Rust fancy-regex）放在核心，不泄漏 syntect 类型。
- **D33**：软换行放 **UI 层**（gpui `shape_text` + 视觉行），核心保持无 GUI；显示列↔字节换算下沉核心可单测。
- **D34**：公式/图表「解析在 `mazha-md`、归类在 `mazha-core`、真排版留渲染层」——`$$…$$`→`MathBlock`/`BlockWidgetKind::Math`，`$…$`→`SpanRole::Math`+`RunStyle::Math`，` ```mermaid `→`Chart{lang}`；导出按字面量（Typst **不**进 `$…$`，R8）。
- **D35**：脚注「解析在 `mazha-md`、样式/导出按可见文本、真渲染留渲染层」——行内 `[^label]`（`FootnoteMarker` 隐藏 + `FootnoteRef` 可见）、块级 `[^label]: body`→`BlockKind::FootnoteDef`；续行缩进 ≥4 且恰好剥 4 空格；`RunStyle::Footnote`（不加 `BlockWidgetKind`）；HTML `sup.fnref`/`div.footnote`、Typst `#super[…]`（**不**用 `#footnote`）、DOCX 纯文本；跳转/编号/文末归集留后续。
- **D53**：图片**本地真实渲染**——核心保留 `dest`、独占行产出 `BlockWidgetKind::Image`；桌面用 GPUI `ImageAssetLoader` 按文档目录加载本地文件，等比缩宽不放大、光标进入显示原文；缺失/行内图片仍占位。
- **D54**：**远程图片**——`http(s)://` 映射为 `Resource::Uri`，桌面启动安装 `reqwest` HTTP 客户端（GPUI 默认 `NullHttpClient`）；下载失败/不支持协议仍回退占位。
- **D55**：**内联 data URI 与协议相对图片**——`data:image/...` 支持 base64/百分号编码，MIME 映射 `ImageFormat` 后复用 GPUI 解码缓存；`//host/path` 本地按 `https://` 加载；坏数据仍占位。
- **D56**：**内联 HTML 标签最小解析**——`parse_inline` 隐藏白名单标签（`font/span/b/strong/i/em/u/s/del/strike/code/mark/br`），内部递归解析并映射强调角色；`<font>/<span>` 的 `color`/`style` 颜色经 `SyntaxSpan.color → TextRun.color` 透传到桌面前景色；结束标签只在当前行内区间查找，未闭合按原文显示；`<br>` 记录为零宽 `HtmlBreak`，供 D66 表格强制换行。
- **D57**：**超宽表格列宽压缩 + 单元格软换行**——自然宽度超可用宽度时不再整表回退 `|` 流式，而是在 `[最小内宽, 自然宽]` 间线性压缩列宽，单元格 `shape_wrapped_line` 软换行；行高按最大视觉行数计算，命中/光标/选区逐视觉行换算；极窄或含中文合成斜体时仍回退流式。
- **D58**：**标签切换统一装载 + 侧栏拖动缩放/文件名省略号**——所有切换入口统一走 `switch_tab`；`sync_current_tab` 用 move 把当前 `Document` 回写标签（保留撤销历史），`activate_tab` 装载目标文档并重建布局/滚动/查找态。侧栏宽度 160–420px 可拖动，长文件名用 `truncate()` 显示末尾省略号。
- **D59**：**浅灰选中态 + 大纲跳转置顶 + 标签关闭按钮 + 底部状态栏**——新增 `Palette.selected_bg`（前景 alpha 0.12）只用于活动标签/侧栏当前项，文本选区保持蓝色；大纲跳转记录 `scroll_to_source_top`，prepaint 按目标视觉行顶边强制置顶并按内容总高 clamp；每个标签增加 `✕`（单标签隐藏），`close_tab(idx)` 支持关闭非当前标签；根布局改为「主内容行 + 24px 全宽状态栏」，文件信息/行数/光标数/快捷键移到底部。
- **D60**：**macOS Dock 图标**——`assets/appicon.png`（1024×1024）用 `include_bytes!` 嵌入，启动时经 AppKit `NSData`/`NSImage` 解码，在主线程调用 `NSApplication.setApplicationIconImage`；仅设置运行中应用图标，正式 `.app` 分发由 D61 补齐。
- **D61**：**macOS `.app` 正式打包**——`macos/Info.plist` + `scripts/bundle-macos.sh`：从 PNG 生成全套 `.icns`，写入 Bundle ID/显示名/版本，复制 release 二进制，默认 ad-hoc 签名、可切 Developer ID，输出 `码札.app` 与 zip；`notarize-macos.sh` 负责 Developer ID 公证 + 装订 + Gatekeeper 校验（需外部 Apple 凭据）。
- **D62**：**桌面外观设置**——`settings.rs` 用 serde 持久化主题（`system/light/dark`）、正文字体/等宽字体、正文字号/等宽字号；启动时应用主题，`System` 订阅窗口 appearance；正文布局改用 `DocView.font_size`，行高取正文与等宽较大者避免行内代码压行；命令面板与快捷键（`Cmd+=`/`Cmd+-`、`Cmd+Option+=`/`Cmd+Option+-`、`Cmd+Shift+L`、`Cmd+Option+0`）调整，状态栏显示字号/主题；`MAZHA_SETTINGS_PATH` 可覆盖设置文件路径。
- **D63**：**macOS 原生应用菜单**——通过 GPUI 注入「码札/文件/编辑/视图/窗口」，标准剪贴板命令走 `OsAction`，菜单与快捷键、主题勾选及异步对话框互锁统一。
- **D64**：**HTML 音视频与 macOS 播放**——单行 `<video>`/`<audio>` → `MediaMarker`/`WidgetKind::Media`；桌面 AVPlayer 懒创建，视频取 32BGRA 帧交 GPUI 自绘并支持 seek，音频为固定 50×50 播放/暂停按钮 + 当前/总时长 + 加载指示，结束时归零并可重复播放；HTML/EPUB 保留标签，PDF/DOCX 占位。网络链接已实机验证，本地媒体仍需回归。
- **D66**：**表格强制换行、列对齐与边框尺寸拖动**——表头规则保持 GFM；单元格 `Shift+Enter` 插入 `<br>` 并按零宽换行点分段排版；分隔行 `:---`/`:---:`/`---:` 控制换行后每个视觉行左/中/右对齐；内部竖线调整相邻列、右外边框调整整表宽度、横边框调整行高、右下角双向调整，悬浮显示对应 resize 光标；尺寸只在当前文档会话保存、不写入 Markdown。自绘 resize hitbox 必须在 prepaint 注册，paint 仅设置光标样式（否则 macOS 回调会 abort）。
- **D67**：**剪贴板图片粘贴**——GPUI `ExternalPaths` 中的图片生成路径 Markdown，内存 `Image` 原始字节生成对应 MIME 的 base64 data URI，最后回退文本；多光标倒序替换保持不变。
- **D69**：**脚注交互闭环**——编号与文末归集只属于导出（WYSIWYG 保留标签原文与原位，维护 1:1 偏移映射）；`mazha-md::FootnoteIndex` 按引用首次出现顺序编号（未引用定义排后、重复定义取首个）；桌面 `Cmd+点击` 引用 ↔ 定义双向跳转；导出 `footnotes::prepare` 先重写标签为序号、再把顶层定义按序号移到文末，HTML/EPUB 输出 `<sup class="fnref"><a href="#fn-N">` 与 `<section class="footnotes"><ol><li id="fn-N">`，Typst/DOCX 仅编号+文末定义；嵌套定义只重编号不移动，无回链，编号不写回 Markdown。
- **D76**：**桌面数学公式真实排版（R4 第一步）**——桌面加 `ratex-gpui`（render-only，`render_latex` 输出透明底着色字形图像，与 gpui-kit 共用 `gpui-pre 0.3.9`）；`DocView` 按 `(tex, 字号, DPR, 颜色)` 缓存渲染结果（含失败缓存）；块级 `$$…$$` 拼成一条 TeX、锚点行显示图像（居左对齐正文文本起点、不画背景/边框）其余行高度归零，点击图像矩形（布局阶段记录 `DocLayout::math_boxes`）把光标放进块内、整块按原始源码逐行显示（含 `$$`，走 `raw_src` 1:1 映射）移开恢复；`collect_inline_texes` 排除块级公式行避免其 TeX body 被误作行内公式透明化；行内 `$…$` 的 `RunStyle::Math` 文本转透明占位（`math_inline_draw_geom` 裁 RaTeX 透明留白并按槽宽绘制，防止压到右侧文字） + paint 阶段定位叠加图像（OffsetMap/光标/选区不变）；光标进入与源码模式显示原文，失败回退字面量；核心/导出不改。
- **D77**：**桌面 Mermaid 图表真实渲染（R4 第二步）**——桌面加 `mermaid-rs-renderer 0.3.1`（render-only，纯 Rust `render(src)` 产 SVG，关 CLI/内置 PNG）；SVG 经 GPUI `SvgRenderer::render_single_frame` 栅格化（逻辑尺寸除 `SMOOTH_SVG_SCALE_FACTOR`）；按 DSL 源码缓存（含失败缓存，与主题/DPR 无关）。渲染态源码转透明占位、锚点行显示图像（按正文宽等比适配）其余行归零，`chart_boxes` 命中图像进 DSL 编辑态移开恢复；失败/不支持语法回退占位面板；核心/导出不改。配色固定 mermaid 默认主题。0.1.103 实机细化：SVG 背景设 `transparent`、paint 不画弱底/描边（去割裂感）；内置示例改无回边分叉——mermaid-rs 0.3.1 对回边流程图菱形端口落边中点、标签偏离连线（上游缺陷，用户文档含回边仍会复现）。已实机目视反馈。
- **D78**：**专注段落粒度 + 打字机中心带细化**——专注范围由「当前单个逻辑行」改为「光标所在段落」：块级 widget 整块高亮，普通行向上下扩展连续文本行（空行/标题/列表/任务/引用/代码块/表格/分割线/块 widget 行截断），标题等带块级语义的行只高亮自身（桌面纯函数 `focus_line_range` + 单测）；打字机滚动由「每次强制居中」改为「中心带」：光标中心落在视口中间 50% 内不滚动、越出才滚回带边缘（纯函数 `typewriter_scroll_y` + 单测，滚动上界 clamp 仍把正偏移截到 0、内容不上移露白）；大纲/查找跳转置顶（D59）不变。核心/导出/源码模式不改。
- **D79**：**中文斜体合成优化（D43 遗留）**——轮廓按 `(字符, 字号)` 缓存为本地坐标路径（`oblique::cached_path` / `place_path`），paint 阶段只平移顶点与包围盒，不再逐字取 CoreText；绘制原点按 gpui 原生字形公式校正：叠加形状化字形 `position.y` 微调（`glyph_y_for_index`）+ 按 DPR half-toward-zero 设备像素对齐（`snap_origin`），消除与透明占位 run 不足 1px 的错位。不做 Metal 着色器（风险高）。核心/导出/其它平台不改；抗锯齿算法差异仍存在。
- **D80**：**合成斜体中文发虚根治**——查 gpui-pre 0.3.9 确认路径着色器的解析式 AA 因 `build_path` 写死 `st=(0,1)` 而失效，只剩 ≤4× MSAA、无灰阶 AA/字干加粗，1px 字干发虚。macOS 改由 CoreText `CTFontDrawGlyphs` + 剪切文本矩阵直接栅格化倾斜字形到 DPR 分辨率位图（`oblique::cached_glyph`），读 alpha 配 span 颜色烘成 `RenderImage` 交 `paint_image` 1:1 叠加；位图几何抽纯函数 `shear_ink_bounds`/`raster_geometry`/`raster_placement`，宽高/原点都取整数设备像素避免二次采样模糊；CJK 透明占位 advance 不变（OffsetMap/光标/选区映射不受影响）。按 `(字符, 字号, DPR, 颜色)` 缓存（含失败缓存 + 容量上限），栅格化失败回退 D79 轮廓路径；非 macOS 仍无合成斜体。核心/导出/其它平台不改，已由用户实机验收通过。
- **D81**：**脚注增强（D69 遗留）**——HTML/EPUB 引用输出唯一锚点 `fnref-{源字节偏移}`（多次引用各自独立），文末 `<li id="fn-N">` 追加 `↩` 回链到首个引用（与桌面 `footnote_jump` 一致）；`footnotes::prepare` 递归收集**所有** `FootnoteDef`（含引用/列表内嵌套），剥离容器前缀后统一归集文末；`mazha-md` 修正列表项续行缩进解析以支持嵌套定义续行。桌面/核心/其它平台不改。A（软换行横向滚动）评估为高风险坐标体系重构未选，记入 SPEC D81。
- **D82**：**桌面导出四格式补齐 + PDF 缺图降级（用户报告修复）**——桌面「文件」菜单/命令面板补齐 HTML/DOCX/EPUB，`ExportFormat`/`ExportTarget` 与纯函数 `export_target`/`export_title`/`pdf_base_dir`/`render_export` 统一对话框参数与渲染分派，`Cmd+E` 仍 PDF；PDF 用 `PdfExporter::with_options` 以当前文档父目录为 `base_dir`，导出 crate 的 `pdf_image_path`/`replace_unresolvable_images` 把缺文件/远程/data/绝对路径图片降级为占位文字（不阻断 Typst 编译，编辑器源文本不变）；内置 SAMPLE 移除不存在的 `assets/sample.png` 引用。远程/内联图片不内嵌进 PDF、HTML/EPUB 不打包资源为既有 MVP 限制；`IMKCFRunLoopWakeUpReliable` 为 macOS 输入法无害日志，不修。
- **D83**：**导出不阻塞窗口 + 状态栏反馈**——桌面 `export_document` 的 CPU 密集段（公式/图表渲染、`render_export_with_assets`、`std::fs::write`）改投 `cx.background_executor().spawn`，`cx.spawn` 只驱动异步对话框与结果派发；`DocView` 增 `export_status`/`export_status_seq`，状态栏显示「正在导出 X…／已导出 X：文件名／导出 X 失败：原因」，进行中/成功用链接色、失败用强调色，完成态 6s 后按序号自动清除、取消对话框立即清除；文案与过期判定为纯函数 + 单测。不引入 toast 框架与全局 Loading 弹窗；导出的是点击时文档快照，期间继续编辑不影响本次文件。
- **D84**：**导出显示渲染后的公式与图表**——导出 crate 保持无 GUI：`render_assets` 纯函数按 `mazha-md` 块模型收集 TeX/mermaid 请求，`RenderAssets` 由桌面用 RaTeX `render_latex_to_svg`（内嵌字形轮廓）与 mermaid-rs（透明背景）填充；`prepare_renderables` 只在导出副本倒序替换为图片 Widget（缺资产保留原字面）。HTML/EPUB 用 `data:image/svg+xml;base64` 复用既有 `<img>`，PDF 用 `export_with_assets` 将 SVG 存入 `ExportWorld` 内存虚拟文件供 Typst `#image` 矢量嵌入（先缺图降级再注入，顺序不可颠倒）；DOCX 因 docx-rs 1.1 无图片 API 维持字面并标注限制。编辑器源文本/OffsetMap/光标/选区零改动。
- **R8**（PDF 真实文档回归）/ **R10**（`platforms/*` 独立 workspace，须单独门禁 + 同步独立 `Cargo.lock`）：见 SPEC §13。

## 依赖方向（不可违反）

- `mazha-core → mazha-md`、`mazha-core → mazha-store`、`mazha-core → mazha-crdt`（core 是共享核心顶层）
- `mazha-md` **不得**反向依赖 `mazha-core`
- 所有核心 crate **不得**依赖任何 GUI（CI 校验）

## 测试策略（已建立）

- **增量 == 全量**逐字段比对：块缓存（`reparse`）、投影 + OffsetMap（`Projection::refresh`）、编辑序列
- **CRDT 双端收敛**：fork → 双端编辑 → 合并 → 文本一致
- **序列化**：结构保持 + 逐字节往返 + 幂等（`to_md(to_md(x)) == to_md(x)`）
- **全管线集成**：`full_pipeline_all_features` 单文档覆盖全部语法 + 编辑后投影一致
- **容器前缀**：嵌套（引用/列表内）代码块/公式的片段起点与序列化前缀不串扰
- 先写测试再实现（尤其光标/偏移映射，R2）

## 下一步（建议顺序）

1. **Phase 4 余项**：**文件树层级化（D70）、拖拽移动/新建文件夹（D71）、展开状态持久化（D72）、拖拽落点高亮/跨应用拖拽（D73）、全工作区搜索 `Cmd+Shift+F`（D40）均已完成**；**当前文档查找/替换** `Cmd+F` 已于 0.1.49 实现。跨应用拖拽（出站）已于 2026-11-18 实机验收通过：把条目拖出窗口交给系统原生拖拽，落入目标目录时**复制文件**到该目录（原条目保留）；**入站**外部拖拽（从 Finder 拖文件进窗口）已于 0.1.96 落地（D74），Finder→窗口入站拖拽亦已实机验收通过。专注/打字机/源码（代码）模式已于 0.1.90 落地（D68）；**0.1.104 完成首轮打磨（D78）**：专注改段落粒度、打字机改中心带；源码模式下表格/媒体已按源码行显示（预览渲染在 `raw_src` 分支被抑制），剩余为交互细节打磨（如源码模式下列表标记/引用装饰的彻底抑制已由 0.1.90 修复，可继续实机找茬）。
2. ~~**中文斜体合成仍不完美**（D43 遗留）~~ **已于 0.1.105 完成首轮优化（D79），0.1.106 完成抗锯齿根治（D80）**：轮廓缓存/原点校正（D79）保留为回退；主路径改为 CoreText 直接栅格化 12° 剪切字形出 DPR 位图（`oblique::cached_glyph` + `paint_image`），消除 gpui 路径仅 ≤4× MSAA、无灰阶 AA/字干加粗造成的「发虚」；新增 7 条桌面单测（剪切墨迹范围/整数设备像素几何/无效几何回退/DPR 放置/位图缓存键/灰阶 AA 覆盖/顶部右倾）。**剩余风险**：CoreText 对剪切矩阵可能关闭部分 hinting，与原生直立字面仍不可能逐像素一致；`CJK_FALLBACKS` 固定顺序可能与 CoreText 级联字面不同；本机截图已能取到窗口（暗色主题下斜体 CJK 行有 181 级灰阶、ASCII 目测顶部右倾且无重影），**已由用户实机验收通过**（Metal 自定义着色器仍需重写 gpui 场景管线，风险高不选）。
3. **渲染层深化**（R4）：**数学排版已于 0.1.98 完成并在 0.1.99/0.1.100/0.1.101 细化块级公式居左/点击编辑/去装饰/编辑态显 TeX、行内不重叠（D76：`ratex-gpui` 块级/行内公式真实栅格化，已实机截图验收）**；**Mermaid 图表真实渲染已于 0.1.102 完成并在 0.1.103 按实机反馈细化（D77：`mermaid-rs-renderer` → SVG → GPUI 栅格化，替换块级占位面板；透明背景去弱底/描边、内置示例改无回边分叉；已据实机反馈修正待复验）**；R4 剩余为**渲染质量跟踪**（mermaid-rs 早期，观感未完全对齐 mermaid-cli；回边流程图菱形端口/标签偏离连线属上游缺陷、应用层无法改布局算法，用户文档含回边仍会复现，失败已回退占位）；脚注点击跳转/自动编号/文末归集已于 0.1.91 完成（D69）；**导出反向回链与嵌套定义归集已于 0.1.107 完成（D81）**，剩余为多引用逐个回跳/复杂容器前缀等打磨；软换行**横向滚动**（D33 已知限制，0.1.107 评估为高风险坐标体系重构未选，见 D81）。
4. §9.1 剩余非 MVP 项：HTML 块、惰性续行、多段落脚注定义。
5. `mazha-export`：字体子集（Web，Phase 7）；`mazha-crdt`：多文档名册；`mazha-store`：文件监听（Phase 5）。
6. **导出修复（D82，0.1.108）已落地，待用户实机验收**：菜单/命令面板 HTML/PDF/DOCX/EPUB 四格式入口与 PDF 缺图降级已补齐；请分别导出四类文件确认可打开（HTML 浏览器、DOCX Word、EPUB 阅读器、PDF），再打开含相对路径图片的文档导出 PDF，确认真实图片保留、缺图显示「（图片：alt）」占位，最后直接导出内置 SAMPLE 确认不再报 `assets/sample.png` 缺失。远程/data/绝对路径图片在 PDF 中退化为占位为已知限制。
7. **导出体验（D83/D84，0.1.109）已落地，待用户实机验收**：① 导出大文档/PDF 时窗口仍可滚动编辑，底部状态栏先显示「正在导出 X…」、完成后显示「已导出 X：文件名」并在 6s 后消失；取消保存对话框应立刻恢复快捷键提示。② 分别导出含 `$…$`/`$$…$$` 公式与 `mermaid` 图表的文档为 HTML/PDF/EPUB，确认公式/图表为渲染后的图形而非 TeX/DSL 字面；DOCX 仍为字面（已知限制，docx-rs 无图片 API）。③ 非法公式/图表 DSL 应保留原字面且不中断导出。

## 提交历史（近期，共 152 条）

```
PENDING feat(desktop/export): 导出不阻塞与渲染公式图表进导出文件（0.1.109，D83/D84）
dc2bedf feat(desktop/export): 补齐四格式导出与 PDF 缺图降级（0.1.108，D82）
8b74a81 feat(md/export): 脚注导出反向回链与嵌套定义归集（0.1.107，D81）
a5f7b6b feat(desktop): CoreText 直接栅格化倾斜字形根治合成斜体发虚（0.1.106，D80）
dfe036b feat(desktop): 中文斜体合成优化，缓存轮廓并校正绘制原点（0.1.105，D79）
2b5efe3 feat(desktop): 专注段落粒度与打字机中心带（0.1.104，D78）
df78360 fix(desktop): Mermaid 图表去背景边框并修正示例连线（0.1.103，D77）
9b6af0c feat(desktop): Mermaid 图表真实渲染，替换块级占位面板（0.1.102，D77）
ed19d00 fix(desktop): 块级公式编辑态显 TeX 原文并修行内重叠（0.1.101，D76）
7eadc91 fix(desktop): 块级公式点击进编辑态并去装饰（0.1.100，D76）
daa864b fix(desktop): 块级公式居左对齐正文（0.1.99，D76）
637acda feat(desktop): 数学公式真实排版 RaTeX（0.1.98，D76）
4ca24cd fix(desktop): 侧栏标题栏固定，列表独立滚动（0.1.97，D75）
4b3e9ca feat(desktop): 入站外部拖拽（从 Finder 拖入）（0.1.96，D74）
5213bc1 feat(desktop): 拖拽落点高亮 + 跨应用拖拽 + 全工作区搜索（0.1.95，D40/D73）
be5f7ef feat(desktop): 文件树展开状态持久化（0.1.94，D72）
7711d0a feat(desktop): 文件树拖拽移动 + 新建文件夹（0.1.93，D71）
608c7b2 feat(desktop): 文件树层级化 + 大纲移至右侧面板（0.1.92，D70）
997c3b3 feat(md/export/desktop): 脚注双向跳转与导出编号/文末归集（0.1.91，D69）
9db3c61 fix(desktop): 源码模式隐藏列表与引用渲染装饰（0.1.90）
ddb185e feat(desktop): 专注/打字机/源码模式（0.1.90，D68）
d487082 feat(desktop): 图片点击边界定位与整图删除（0.1.89）
12400bc feat(desktop): 支持从剪贴板粘贴图片（0.1.88，D67）
7b9574e fix(desktop): 表格多行单元格继承列对齐（0.1.87，D66）
0a67ff5 feat(desktop): 表格换行与边框尺寸拖动（0.1.87，D66）
7f6a4cb fix(desktop): 音频加载反馈与结束重播（0.1.86，D64）
6a1c53d feat(desktop): 原生菜单与音视频播放（0.1.84–0.1.85，D63/D64）
8513df6 feat(desktop): 外观设置——主题/字体/字号持久化（0.1.83，D62）
063e3c4 feat(desktop): macOS .app 打包与正式图标（0.1.82，D61）
16b993b feat(desktop): macOS Dock 应用图标（0.1.81，D60）
ed540ca feat(desktop): 浅灰选中/大纲置顶/标签关闭/状态栏（0.1.80，D59）
f64a678 fix(desktop): 标签切换装载 + 侧栏缩放/省略号（0.1.79）
ee4b3cd fix(desktop/md/core): 视觉行布局与超宽表格软换行（0.1.78）
c0e194f feat(desktop/md/core): 内联 HTML 标签与 <font> 样式解析 + 表格分隔行布局修复
07fd742 feat(desktop): 文本选区（拖拽/Shift+点选/全选/双击选词/三击选行）+ 蓝色选区高亮
66db09a feat(desktop): 远程图片加载（0.1.75，D54）
957875e feat(desktop): 本地图片真实渲染（0.1.74，D53）
8417ea6 fix(desktop): 引用块左右越界（0.1.73）
c25637e fix(desktop): 代码块右侧越界（0.1.72）
c09a6ac fix(desktop): 表格外横移第一下落入单元格（0.1.71）
64105e2 fix(desktop): 表格光标格尾视觉提前跳格（0.1.70）
1b6f4bb fix(desktop): 表格内左右方向键跳过隐形字符（0.1.69）
ff6262b feat(desktop): 表格按内容列宽/竖向网格线 + 单元格 Tab 导航（0.1.68）
46b8cf7 feat(desktop): 表格观感优化（表头/分隔行/单元格留白，0.1.67）
7daced0 fix(core): 带嵌套内容的新列表项即时显示序号（0.1.66）
6fbb83c feat(core): 引用内列表回车续行（0.1.64）
1d2b2e2 feat(desktop): 列表/任务回车续行，新项图标即时显示（0.1.63）
bb953df fix(desktop): 代码块光标/选区与文本同起点（0.1.61）
0506727 feat(desktop): 中文斜体倾斜 + 行内代码底色 + 代码块浅底与就地显源码（0.1.60）
318f7d4 docs: 归总 HANDOFF/SPEC（0.1.59 行内格式局部展开 + 引用块嵌套修正）
dd40eff feat(desktop): 光标在粗体/斜体/删除线/行内代码内展开标记（同链接展开逻辑）
7c381a3 fix(desktop): 恢复嵌套引用背景和竖线，仅最外层加上下 padding
aba26a4 fix(desktop): 嵌套引用不单独画背景和左竖线（最外层统一处理）
9d1c56c fix(desktop): 引用块仅最外层加上下 padding（嵌套引用不加）
2b28968 docs: 更新 HANDOFF/SPEC（0.1.58）
8d3500d fix(desktop): 引用块更浅底色并上下加 padding（0.1.58，Phase 4）
0b95e0a fix(desktop): 引用块底色浅色主题不可见（0.1.57，Phase 4）
2b005dd feat(desktop): 链接局部展开/Cmd+点击 + 配色（0.1.56，Phase 4）
2624097 fix(desktop): 滚动/命中/IME 修复（0.1.55，Phase 4）
5ddbf5f fix(desktop): 原生对话框改用异步 AsyncFileDialog（D41 修正，Phase 4）
c46e7b3 fix(desktop): 布局/光标/原生对话框 panic（D41，Phase 4）
f605595 docs: 归总 HANDOFF + SPEC D40（全工作区搜索列低优先规划）
e63ea11 feat(desktop): 大纲/目录导航（侧栏标题列表+点击跳转，D39，Phase 4）
62b80e1 feat(desktop): 文件 CRUD（重命名/删除/移动，D36 遗留，Phase 4）
81b0fb6 feat(desktop): 命令面板 Cmd+P（D38，命令集 + 过滤 + 执行）
060f96f docs: Phase 4 命令面板设计（D38）
7c7ffdb feat(desktop): 多标签页支持（标题栏/切换/关闭/新建，D37 基础）
08323fa feat(desktop): 多标签页快捷键（Cmd+1..9/0、Cmd+W/Shift+W）
ae14bb3 feat(desktop): 文件树新建 Markdown（最小）
fb9ed20 feat(desktop): 文件树点击切换文档（最小可用）
cb8fd3e docs: Phase 4 多标签页设计（D37）
23f8012 feat(desktop): 文件管理首增量（Cmd+O/S/Shift+S + 脏状态，兼容 Workspace）
1c2a393 feat(md): 脚注解析与建模（[^label] 引用 + [^label]: body 定义，0.1.52，Phase 3）
6c7ea52 feat(md): 公式/图表解析与建模（$$…$$ / $…$ / mermaid 归类，0.1.51，Phase 3）
1f42f08 fix(export): 多行代码块导出丢失换行（HTML/Typst，R8 同类缺陷）
b6fd01b feat(core): 查找/替换（find_all/replace_all）+ 桌面 Cmd+F 查找条（0.1.49，Phase 3）
```
c8136b7 feat(core): 代码块语法高亮（syntect Highlighter）+ 桌面着色（D32，Phase 3）
25a4670 feat(core): 表格增删行列（TableOp/edit_table）+ 桌面快捷键（D31，Phase 3）
3c497ff feat(desktop): 块级 widget 渲染（代码块面板/表格网格/分割线，Phase 3）
c80a24a feat(core): 块级 widget 模型（BlockWidget/BlockWidgetKind，Phase 3）
fdd1252 feat(desktop): 接入核心文档级 API + 修复历史编译债（D29，Phase 3）
c5d6d72 feat(core): 文档级跨行坐标映射（DocPos/选区区间，Phase 3）
05e8086 test(export): PDF 真实文档回归（R8）+ 修复行内代码反引号
398f20f feat(export): 导出主题/模板（Theme/Template，D28）
fc4721f feat(export): EPUB 导出（Zip + XHTML，D27）
6922f6a feat(export): DOCX 导出（docx-rs，D26）
917d9ce chore: update Cargo.lock for mazha-export + rfd deps
531bdc7 feat(desktop): PDF 导出（Cmd+E）
72ee12a feat(desktop): 多光标支持
2a9c1c8 feat(desktop): 编辑命令接入（commands/apply_and_sync）
9e188db feat(desktop): IME 支持 + EntityInputHandler 完整实现
2e9a5af feat(desktop): 自写 Element + shape_line 连续布局 + 跨行选区
9e24880 feat(desktop): 光标/点击定位 + WYSIWYG 标记显隐（光标行原文，其余投影隐藏）
13e0295 feat(desktop): Phase 2 起步——GPUI 只读投影渲染（可运行）
e14e14f feat(export): PDF 编译——Typst World 实现 + to_pdf（PDF 管线第二步）
d60189c docs: HANDOFF 全面刷新（Phase 1 完成状态）
```

> 更早提交见 `git log`；历史累计 = 151（含本次）。

- 2026-11-18（`PENDING`，0.1.109）：**导出不阻塞 + 状态栏反馈 + 渲染后公式/图表进入导出文件（D83/D84，用户实机验收反馈）**：① **需求 1（不阻塞）根因**——D82 的 `export_document` 虽用 `AsyncFileDialog` + `cx.spawn`，但 gpui-pre 0.3.9 的 `cx.spawn` 跑前台 executor；对话框 await 后同步执行 `PdfExporter::with_options`（系统字体扫描）与 `typst::compile`，CPU 密集任务冻结窗口，且成功/失败只 `eprintln!`，用户看不到反馈。**方案（D83）**——选文件前即 `begin_export_status` 标记进行中，把「资源收集 + 渲染 + 排版 + 写文件」整体投到 `cx.background_executor().spawn`（future 为 `Send + 'static`），前台只接收结果并 `this.update` 回写状态；底部状态栏显示「正在导出 X…／已导出 X：文件名／导出 X 失败：原因」，完成态 6s 后按递增序号自动清除，取消对话框立即清除；文案/过期判定抽纯函数并配单测。**不采用**用户备选的全局 Loading 弹窗，也不引 toast 框架；导出的是点击时快照，期间继续编辑不影响本次文件。② **需求 2（公式/图表）可行性结论**——`ratex-gpui 0.4.1` 的 `render_latex_to_svg`（parse→layout→ratex_svg，内嵌字形轮廓）是 gpui-free 纯函数，但 crate 本身依赖 `gpui-pre`，不能进无 GUI 的导出 crate；`mermaid-rs-renderer 0.3.1` 纯 Rust 产 `<svg>` 且可设透明背景；Typst 0.15 原生识别 SVG 图片，可由 `ExportWorld` 从内存虚拟文件供字节；`docx-rs 1.1.2` 无图片嵌入 API。**方案（D84）**——渲染留桌面，导出 crate 只做纯文本桥接：新增 `render_assets.rs`（`collect_render_requests`/`prepare_renderables`、`RenderAssets`/`AssetTarget`/`RenderRequest`/`PreparedMarkdown`），桌面按请求键调 RaTeX/mermaid 产自包含 SVG，在**导出副本**倒序替换公式/图表区间为图片 Widget（缺资产保留原字面）；HTML/EPUB 用 `data:image/svg+xml;base64` 复用既有 `<img>`，PDF 用 `export_with_assets` 把 SVG 放进 `ExportWorld` 内存表并以 `__mz_asset_N.svg` 喂 Typst `#image`（矢量嵌入）；顺序固定为「真实缺图降级 → 注入虚拟图片 → Typst 编译」，否则虚拟文件会被缺图判定吃掉；DOCX 维持字面并明确标注限制，不引入第二套 OOXML 图片实现。③ **安全性**——编辑器 `Document`/`OffsetMap`/光标/选区/源文本零改动；块级 `$$…$$` 仍须独占一行才按 D34 解析，行中间 `$$x$$` 按普通文本保留（与编辑器一致）。④ **文件**——`SPEC.md`（D83/D84 + §15 0.1.109）、`HANDOFF.md`、新增 `crates/mazha-export/src/render_assets.rs`、`crates/mazha-export/src/{lib.rs,pdf.rs,Cargo.toml}`（base64 依赖、虚拟资源表、`to_pdf_with_assets`）、`platforms/desktop/src/main.rs`（状态栏/后台导出/SVG 收集与渲染/单测）；未改 `docx.rs`/核心/其它平台。⑤ **验证**——根 workspace fmt/clippy/test 全绿（**565** 项：core 204、md 177、export **155+3**、crdt 11、store 12、ffi/plugin/wasm 各 1），桌面 fmt/clippy/test/build 全绿（**91** 项，新增状态文案、过期判定、真实 SVG 收集、HTML data URI、非法 DSL 降级及 PDF 虚拟资源注入单测；链接器 `__eh_frame section too large` 警告为既有无害提示）。⑥ **待用户实机验收**——导出大文档/PDF 时窗口仍可操作且状态栏文案正确、6s 自动消失；HTML/PDF/EPUB 中公式与 mermaid 图表呈渲染图形；DOCX 字面为已知限制。截图环境不稳定，未做自动目视验收。
- 2026-11-18（`dc2bedf`，0.1.108）：**桌面导出四格式补齐 + PDF 缺图降级（D82，用户报告修复）**：① **根因**——用户报告「菜单里只有导出 PDF 一项，且导出 PDF 失败：`file not found (searched at /assets/sample.png)`」。调查确认两件事：`mazha-export` 早已有 `to_html*`/`to_docx`/`to_epub*`/`to_pdf*` 全套 API，桌面 `main.rs` 却只把 `to_pdf` 接到 `PaletteAction::ExportPdf`（菜单/面板/快捷键/分发均只有一项）；内置 `SAMPLE` 写了 `![示例图片](assets/sample.png)` 但磁盘无此文件，一次性 `to_pdf` 走 `PdfExporter::default()` → `PdfExportOptions::base_dir = None`，`ExportWorld::resolve` 返回 `FileError::NotFound`，Typst 编译中断。`IMKCFRunLoopWakeUpReliable` 经确认是 macOS 输入法无害日志，按要求**不修**。② **导出面**——桌面新增 `ExportFormat::{Html,Pdf,Docx,Epub}`、`ExportTarget` 与纯函数 `export_target`/`export_title`/`pdf_base_dir`/`render_export`，并用通用 `export_document` 复用异步 `AsyncFileDialog`（D41）与错误上报；「文件」菜单与命令面板补齐 HTML/DOCX/EPUB 三项，`Cmd+E` 仍只对应 PDF；各 `to_*` API 内部已做脚注规整（D69/D81），桌面不重复调用。③ **资源根与降级**——PDF 改用 `PdfExporter::with_options`，`base_dir` 取当前文档父目录（纯函数 `pdf_base_dir`，无路径/裸文件名返回 `None`）；导出 crate 新增 `pdf_image_path`（远程、`data:`、`mailto:`、`file://`、协议相对与宿主机绝对路径一律判不可解析）与 `replace_unresolvable_images`（回调注入文件存在性、按源偏移倒序替换图片 Widget 为「（图片：alt）」），在 `PdfExporter::{export,export_with}` 入口对独立副本预处理，缺图/远程图片不再阻断整篇 Typst 编译；代码块中的相同文本不是图片 Widget，不受影响，编辑器源文本/OffsetMap 也不变。④ **内置 SAMPLE**——移除不存在的 `assets/sample.png`，改为纯文字说明图片用法，消除开箱即用的报错源。⑤ **取舍/限制**——PDF 中远程/data/绝对路径图片退化为占位（Typst World 只能从 `base_dir` 读相对资源，不内嵌、不写临时文件）；HTML/EPUB 仍原样输出 `<img>` 但不打包资源（既有 MVP 限制，读端可能显示破图）；`parse_image_typst` 改 `pub(crate)` 复用同一套 `![alt](dest)` 解析规则。⑥ **验证**——根 workspace fmt/clippy/test 全绿（**557** 项：core 204、md 177、export **147+3**、crdt 11、store 12、ffi/plugin/wasm 各 1），桌面 fmt/clippy/test/build 全绿（**87** 项，新增 6 条桌面单测 + 4 条导出 crate 单测，并把原「缺图报错」回归改为「降级成功」断言；链接器 `__eh_frame section too large` 警告为既有无害提示）；待用户实机验收四类导出文件可打开、含图 PDF 真实图片保留与缺图占位、内置 SAMPLE 导出成功。

- 2026-11-18（`8b74a81`，0.1.107）：**脚注增强：导出反向回链 + 嵌套定义归集（D81，D69 遗留）**：① **方案选择**——先评估两项候选：A「软换行横向滚动（D33）」需给 `DocLayout` 的 paint/命中/光标/选区/IME/块级装饰/数学图表图片叠加层统一补 x 偏移并扩展内容宽度，属高风险坐标体系重构，**不选**；B「脚注增强」主要改核心纯逻辑、可单测，风险低，选 B 并在 SPEC D81 记录理由。② **反向回链**——HTML/EPUB 引用输出 `<sup class="fnref"><a id="fnref-{源字节偏移}" href="#fn-N">N</a></sup>` 唯一锚点（多次引用各自独立，避免把序号状态贯穿渲染器各层），文末 `<li id="fn-N">` 追加 `<a class="fnback" href="#fnref-{首个引用偏移}">↩</a>`（多次引用只回跳首个，与桌面 `footnote_jump` 一致）；`DEFAULT_CSS` 增 `.fnback`。③ **嵌套归集**——`footnotes::prepare` 的 `move_definitions` 由「仅顶层」改为递归收集**所有** `FootnoteDef`，按首行 `[^` 前的容器前缀逐行剥离（`> `/列表标记/缩进，续行保留 4 空格定义缩进），按序号排序统一归集文末，原块按源偏移倒序整行裁切。④ **解析修正**——`mazha-md` 列表项续行原 `min(indent(nrest), nl.content + marker.len)` 混用相对缩进与绝对偏移，把续行空白剥光致嵌套定义续行脱离定义块，改为 `min(indent, marker.len)` 并补回归单测。⑤ **验证**——桌面已有定义→首个引用 `Cmd+点击` 跳转（D69）不变，`mazha-core`/其它平台不改；新增纯函数单测（前缀剥离、嵌套归集与排序、续行、回链、多次引用唯一锚点），根 workspace fmt/clippy/test 全绿（**553** 项：md 177、export 143+3、core 204 等），桌面 fmt/clippy/test/build 全绿（**81** 项不变）；待用户实机验收（HTML/EPUB 回链跳转、嵌套定义归集观感）。

- 2026-11-18（`a5f7b6b`，0.1.106）：**合成斜体中文发虚根治（D80，实机反馈）**：① **根因调查**——用户实机确认英文斜体、普通中文均清晰，仅合成斜体中文发虚；查 gpui-pre 0.3.9 源码发现 `PathBuilder::build_path` 给所有三角形写入常量 `st=(0,1)`，路径着色器 `fs_path_rasterization` 因 `dpdx(st)=0` 落入 `alpha=1` 分支，**解析式 AA 实际未生效**，边缘只剩路径离屏纹理的 ≤4× MSAA（5 级覆盖），没有 CoreText 的 8bit 灰阶 AA / LCD 亚像素 / 字体平滑（字干加粗），1px 字干呈半透明灰即「发虚」；D79 的轮廓缓存与原点对齐只解决位置，不解决质量。② **方案**——macOS 改由 CoreText `CTFontDrawGlyphs` + `CGContextSetTextMatrix(CGAffineTransform(1,0,k,1,0,0))` 把 12° 剪切交给 CoreText 自身栅格器，按字号×DPR 出 `CGBitmapContext`，读 alpha 作覆盖率、配 span 颜色烘成直 alpha RGBA `RenderImage`，由 `window.paint_image` 1:1 叠加（位图宽高与原点到边缘均取整数设备像素，避免线性采样二次模糊）。③ **布局安全**——CJK 透明占位 run 的 advance 一字未动；`snap_origin` / `glyph_y_for_index` 原点公式沿用 D79；纯函数 `shear_ink_bounds`/`raster_geometry`/`raster_placement` 承担全部几何计算并配单测。④ **缓存/降级**——按 `(字符, 字号, DPR, 颜色)` 线程局部缓存（含失败缓存 + 4096 条上限），栅格化失败回退 D79 `paint_path`（不缺字），非 macOS 仍无合成斜体；D79 轮廓路径保留为兜底。⑤ **验证**——核心 / `mazha-md` / 导出 / 其它平台不改；根 workspace fmt/clippy/test 全绿（548 项），桌面 fmt/clippy/test/build 全绿（**81 项**，新增 7 条：剪切墨迹范围、整数像素几何、无效几何回退、DPR 放置、位图缓存键、灰阶 AA 覆盖、顶部右倾）；macOS 真机栅格化单测确认位图非空、含 0<A<255 的灰阶边缘、且顶部 alpha 质心明显右于底部（剪切生效）。本机截图可用（`screencapture` 取窗口成功，暗色主题下斜体 CJK 行统计 181 级灰阶、ASCII 目测无重影），**已由用户实机验收通过**；不采用 Metal 自定义着色器/重写 gpui 场景管线（风险高）。

- 2026-11-18（`dfe036b`，0.1.105）：**中文斜体合成优化（D43 遗留，D79）**：① **轮廓缓存**——`oblique::cached_path` 按 `(字符, 字号位模式)` 线程局部缓存**本地坐标**（原点 0、基线 0、已施加 12° 剪切）的 `Path<Pixels>`，失败结果同样缓存；paint 阶段用 `place_path` 仅平移顶点与包围盒（不重取 CoreText 轮廓、不重新三角化），跨帧/跨块/跨文档复用，字号变化自动落到新键。选线程局部而非 `BlockGeom` 是因为布局每帧重建，挂在块上无法跨帧复用。② **原点校正**——自绘起点改为与 gpui `paint_line`/`paint_glyph` 一致：x 沿用 `position_for_index`（软换行已扣行首偏移），y 叠加从 `shaped.runs()` 中按首个 `index >= byte` 取到的 `glyph.position.y`（`glyph_y_for_index`），再按 `window.scale_factor()` 做 `(abs(v)-0.5).ceil().copysign(v)` 设备像素对齐（`snap_origin`），修掉透明占位 run 与自绘轮廓不足 1px 的错位。③ 未选 Metal 自定义着色器：需重写 gpui 场景管线、风险高且无法纯函数验证。核心 / `mazha-md` / 导出 / 其它平台不改。新增 4 条桌面纯函数单测（缓存复用同指针且字号变化重取、路径平移只改几何、DPR 对齐含负坐标与 0.5 边界、字形 y 判据），桌面 74 项、根 workspace 548 项，根与桌面 fmt/clippy/test/build 全绿。**限制/待验**：抗锯齿仍是 gpui 路径 st coverage 而非 CoreText 字形栅格化，笔画边缘不可能逐像素一致；字形仍按固定 `CJK_FALLBACKS` 顺序取，个别标点可能与 CoreText 级联字面不同；截图环境不可用（`screencapture` 取窗口空/全黑），未做实机目视验收，待用户对比 Typora 后决定是否继续打磨。

- 2026-11-18（`2b5efe3`，0.1.104）：**专注/打字机模式打磨（D68 首轮细化，D78）**：① **专注范围改段落粒度**——原先只淡化「当前单个逻辑行」，中文写作一段多行（源文本手动换行）会被相邻行误淡化；新增桌面纯函数 `focus_line_range`：光标行属于块级 widget（代码/表格/公式/图表/图片）时整块高亮，普通行向上下扩展连续文本行，遇空行/标题/列表/任务/引用/代码块/表格/分割线/块 widget 行即截断，标题等带块级语义的行只高亮自身（避免标题吞掉其后正文）。② **打字机滚动改中心带**——原先每次光标行几何变化都强制居中、连续移动时视口持续跟随；新增纯函数 `typewriter_scroll_y`：光标中心落在视口中间 50%（半带 = 视口高 1/4）内不滚动，越出才滚回带边缘；滚动上界 clamp 仍把正偏移截到 0（内容不上移露白），大纲/查找跳转置顶（D59）与边界 clamp 不变。核心 / `mazha-md` / 导出 / 源码模式不改；新增 2 条桌面纯函数单测（70 项），根 workspace 548 项、桌面 fmt/clippy/test/build 全绿。已由用户实机验收通过。

- 2026-11-18（`df78360`，0.1.103）：**Mermaid 渲染观感按实机反馈细化（D77 细化）**：① **去背景/边框**——`render_chart_preview` 改用 `render_with_options` 把 `theme.background` 设为 `transparent`（mermaid-rs 默认在 SVG 根节点画白色 `<rect>`，深色主题下是突兀白块），paint 阶段不再画 `palette.subtle` 弱底与 `paint_rect_border`（节点自带浅底/描边，同 D76 公式块）；新增透明背景单测断言（角落 padding 像素 alpha=0）。② **连线起点/标签位置**——内置示例 `B -->|否| A` 改为无回边分叉 `B -->|否| D[重试]`：dump SVG 实测确认 mermaid-rs-renderer 0.3.1 对菱形 branching shape 在 degree≥3 时用边中点作端口（「否」线起点 `193.505,75.600` 落在菱形下边中点而非顶点）、回边标签用路由前锚点（「否」字甩到 `x=53.51,y=111.79` 脱离连线）；这是上游布局缺陷，0.3.1 为最新版无修复，应用层只能靠无回边写法规避，用户文档含回边图仍会复现。无回边分叉时两条出边精确从菱形右顶点 `216.11,77.31` 出发、标签贴线。核心/导出不改。桌面 68 项、根 workspace 548 项门禁全绿。已据用户实机反馈修正，待用户复验。

- 2026-11-18（`9b6af0c`，0.1.102）：**桌面 Mermaid 图表真实渲染（R4 第二步，D77）**：桌面新增 `mermaid-rs-renderer 0.3.1`（render-only，纯 Rust `render(src)` 产 SVG，关 CLI/内置 PNG），SVG 经 GPUI `SvgRenderer::render_single_frame` 栅格化（逻辑尺寸除 `SMOOTH_SVG_SCALE_FACTOR`）；`DocView` 按 DSL 源码缓存（含失败缓存，与主题/DPR 无关）。渲染态 mermaid 块源码转透明占位、锚点行显示图像（弱底 + 圆角 + 描边，按正文宽等比适配）其余行归零；`DocLayout::chart_boxes` 命中图像把光标放进围栏块显示 DSL 原文，移开恢复；失败/不支持语法回退弱底 + 语言标签占位面板。内置 `SAMPLE` 增加 mermaid 示例。核心/导出不改，导出仍按字面量。桌面新增 DSL 提取/缓存键/SVG 栅格化出图/命中框/文本透明化 5 条单测（68 项）；根 workspace 548 项门禁全绿。截图环境不可用（`screencapture` 取窗口空/全黑），未做实机目视验收，由渲染管线单测（非空像素断言）验证。

- 2026-11-18（`ed19d00`，0.1.101）：**块级公式编辑态显 TeX + 行内不重叠**（D76 细化）：① 块级公式编辑态改为整块按原始源码逐行显示（含 `$$` 定界行，走 `raw_src` 通道做 1:1 字节映射），修掉「点进公式后仍看不到 TeX 原文」——根因是块 body 同为 `RunStyle::Math`，被 `inline_texes` 收进预渲染集合后又被当作行内公式透明化；新增纯函数 `collect_inline_texes` 排除块级公式行与代码块行。② 新增纯函数 `math_inline_draw_geom`：按透明占位槽起止位置算可用宽度，裁掉 RaTeX 左右各 8px 透明留白后再画，字形区宽于槽位才等比缩小，防止图像越过槽宽压到右侧普通文字。新增 2 条桌面单测（行内绘图几何边界、块级 TeX 不混入行内集合）。桌面 fmt/clippy/test/build 全绿（63 项）；根 workspace fmt/clippy/test 全绿（548 项）；实机截图确认块级点击显示含 `$$` 的完整 TeX、移开恢复公式图像、行内公式与右侧文字有间隔无重叠。

- 2026-11-18（`7eadc91`，0.1.100）：**块级公式点击编辑 + 去装饰**（D76 细化）：渲染态 `$$…$$` 不再画背景/边框（`WidgetDecorKind::Math` 空操作）；新增纯函数 `math_hit_rect` 与 `DocLayout::math_boxes`（图像矩形 + 块源范围，矩形与 paint 绘制位置一致），`DocView::hit_source` 先按图像矩形命中并把光标落到块源范围起点，下一帧整块显示 TeX 原文（同行内公式），移开恢复渲染态；解决透明占位 run 在零高定界行旁的视觉行命中落到块外空行、点了图像却进不了编辑态的问题。新增 1 条桌面单测（命中矩形边界）。桌面 fmt/clippy/test/build 全绿（61 项）；实机截图确认点击进源码、移开恢复公式图像、无边框。

- 2026-11-18（`daa864b`，0.1.99）：**块级公式居左对齐**（D76 细化）：把 `$$…$$` 块级公式图像的绘制坐标由「正文可用宽度内居中」改为 `text_x`（居左对齐正文文本起点，与图片块一致），删除 paint 阶段居中偏移计算（`avail`/`x`）；RaTeX 图像自带四周留白，不紧贴 widget 外框。仅调整绘制坐标，OffsetMap/光标/选区/缓存/导出均不变；行内公式仍垂直居中叠在文本行。桌面 fmt/clippy/test/build 全绿（60 项）；实机截图确认公式字形逻辑 x≈249（正文起点 240 + RaTeX 留白），积分/上下限/分数横线完整。

- 2026-11-18（`637acda`，0.1.98）：**桌面数学公式真实排版（R4 第一步，D76）**：桌面新增 `ratex-gpui`（render-only，与 gpui-kit 共用 `gpui-pre 0.3.9`），`$$…$$` 块与 `$…$` 行内在渲染态由 RaTeX 栅格化为透明底、字形着色的公式图像并叠加显示；TeX 文本在 gpui 侧转透明占位（保留排版/光标/选区/偏移映射），块级公式拼成一条 TeX、锚点行显示图像其余行高度归零，行内公式按显示字节区间定位叠加；`DocView` 按 `(tex, 字号, DPR, 颜色)` 缓存渲染结果（含失败缓存），主题/字号变化时清空；光标进入公式与源码模式显示原文，渲染失败回退字面 TeX。核心/导出不改，mermaid 真渲染留后续。内置 SAMPLE 增加行内/块级公式示例。桌面新增缓存键/TeX 拼接/RaTeX 栅格化/透明占位/定界符展开 5 条单测，`SAMPLE` LaTeX 反斜杠转义修正。桌面 fmt/clippy/test/build 全绿（60 项）；根 workspace fmt/clippy/test 全绿（548 项）。待实机目视验收。

- 2026-11-18（`4ca24cd`，0.1.97）：**侧栏标题栏固定**（D75）：把「资源管理器 / ＋文件 / ＋文件夹」标题从滚动列表中拆出，作为 `#sidebar` 直接子节点（`h(28px)` + `flex_none()`），条目列表改由 `sidebar-list`（`flex_1` + `min_h(0)` + `overflow_y_scroll`）承载，列表过长时标题与新建入口始终可见；标题的根目录拖拽落点（`TreeDrag` / `ExternalPaths`）保持不变。桌面 fmt/clippy/test/build 全绿（55 项）；根 workspace fmt/clippy/test 全绿（548 项）。

- 2026-11-18（`4b3e9ca`，0.1.96）：**入站外部拖拽（从 Finder 拖入）**（D74）：根元素、侧栏标题与目录行接受 GPUI `ExternalPaths` 拖拽负载并高亮落点，与 `TreeDrag` 落点共存；拖入条目复制进目标目录（同名递增编号、目录递归、跳过符号链接），Finder 原件不动；`.md`/`.markdown` 自动打开为标签，编辑区拖入的非 Markdown 文件按当前文档目录生成相对链接（图片 `![ ]`、其它 `[ ]`）并插入所有光标。新增纯函数 `plan_imports`/`unique_import_path`/`copy_entry`/`dropped_link_markdown` 与 3 条桌面单测，`insert_text_at_cursors` 抽出供粘贴复用。桌面 fmt/clippy/test/build 全绿（55 项）；根 workspace 门禁全绿（548 项）。Finder→窗口入站拖拽已实机验收通过；D73 出站拖拽亦已实机验收通过（拖出窗口→目标目录复制文件）。

- 2026-11-18（`5213bc1`，0.1.95）：**拖拽落点高亮 + 跨应用拖拽 + 全工作区搜索**（D40/D73）：文件树目录行与侧栏标题新增 `drag_over::<TreeDrag>` 高亮（铺 `palette.selection`，非法落点由 `can_drop` 阻止），侧栏标题 `can_drop` 只接受非根层条目；目录/文件行新增 `external_drag_payload`（`ExternalDragPayload::Files`）支持拖出窗口交给系统原生文件拖拽（跨应用/Finder，已于 2026-11-18 实机验收通过：落入目标目录时复制文件，原条目保留）。新增 `Cmd+Shift+F` 全工作区搜索悬浮面板：即时递归扫描工作区 Markdown，按文件分组列出命中行与上下文（命中高亮、按字符截断），`↑↓` 选择、`Enter`/点击打开并选中、`Cmd+Alt+C` 大小写、`Esc` 关闭，上限 500 条标记截断、读取失败计入跳过，命令面板/编辑菜单同步入口。新增纯函数 `collect_markdown_paths`/`search_text_lines`/`tail_chars`/`head_chars`；修复 `mazha-core::find::find_all` 大小写不敏感模式「模式比文本长」切片越界 panic（补回归断言）。桌面新增 3 条单测；桌面 fmt/clippy/test/build 全绿（52 项）。

- 2026-11-17（`be5f7ef`，0.1.94）：**文件树展开状态持久化**（D72）：`Settings` 新增 `tree_expanded`（按工作区根存展开目录列表），`tree_expanded_for`/`set_tree_expanded_for`（空列表删键），`reset_appearance` 保留。`DocView` 启动按工作区根恢复 `expanded_dirs`，`toggle_dir`/`refresh_workspace` 后落盘。新增 1 条设置单测；桌面 fmt/clippy/test/build 全绿（49 项）。

- 2026-11-17（`7711d0a`，0.1.93）：**文件树拖拽移动 + 新建文件夹**（D71）：文件/目录行接入 GPUI `on_drag`（`TreeDrag` + `TreeDragPreview`），目录行与侧栏标题 `sb-h` 作为落点（`can_drop`/`on_drop`），落点经 `move_entry` 移动文件/目录（拒绝自身/后代/同名，跟随更新当前文档路径与展开态前缀）。新增 `new_folder_in_root`/`new_folder_in` 建文件夹并进入重命名态，标题栏改为 `＋文件`/`＋文件夹`，命令面板加「新建 Markdown」「新建文件夹」。文件行打开/目录行折叠改 `on_click` 避免拖拽误触。新增纯函数 `drop_allowed`/`remap_expanded` 与 2 条单测；桌面 fmt/clippy/test/build 全绿（48 项）。

- 2026-11-17（`608c7b2`，0.1.92）：**文件树层级化 + 大纲移至右侧面板**（D70）：文件树改用 `Workspace::list` 递归渲染目录/文件（目录恒显示、按 `expanded_dirs` 展开子项、文件只留 Markdown），目录行 `▸/▾` 展开折叠、`＋` 在目录内新建、`✎` 重命名、`✕` 递归删除，侧栏标题栏 `＋` 在根目录新建；删除/重命名目录同步迁移展开态前缀与当前打开文档路径。大纲从左侧栏移出，改为右侧独立可拖动面板（`outline_width` 160–420px），左侧只保留文件树。新增纯函数 `collect_tree_rows` 与桌面 1 条层级/展开/过滤单测；桌面 fmt/clippy/test/build 全绿（46 项）。

- 2026-11-16（`997c3b3`，0.1.91）：脚注交互闭环（D69）：新增 `mazha-md::FootnoteIndex`（按引用首次出现顺序编号、未引用定义排后、重复定义取首个，提供引用/定义区间查询）与 `Document::footnote_index()`；桌面 `Cmd+点击` 引用标签跳到定义块起点、点击定义标签跳回首个引用。导出新增 `footnotes::prepare` 源文本预规整（标签重写为序号 + 顶层定义按序号归集文末），HTML/EPUB 输出带锚点的 `<sup class="fnref"><a href="#fn-N">N</a></sup>` 与 `<section class="footnotes"><ol><li id="fn-N">`，Typst/DOCX 输出编号与文末定义；无脚注文档逐字节原样直出。限制：嵌套定义只重编号不移动、无回链、编号不写回 Markdown。workspace 548 / 桌面 45，fmt/clippy/test/build 全绿。
- 2026-11-16（本次提交，0.1.90 修复）：源码模式补齐任务列表与引用块渲染态抑制：不再 shape/绘制列表标记与任务勾选 glyph，不注册任务复选框命中区域，不绘制复选框边框/勾选与引用块竖条/背景，仅显示原始 Markdown 文本（如 `- [ ]`、`>`）。桌面 fmt/clippy/test 全绿（44 项）。
- 2026-11-16（`ddb185e`，0.1.90）：新增专注模式/打字机模式/源码模式（D68），设置持久化与菜单/命令面板/快捷键接入；桌面与 workspace 门禁全绿。
- 2026-11-16（`d487082`，0.1.89）：优化桌面端独占图片交互：渲染态点击图片左半区将光标定位到图片源码起点、右半区定位到源码终点（进入编辑态后光标落在对应边界）；光标紧贴图片末尾时 `Backspace`、紧贴图片开头时 `Delete` 直接删除整段图片源码（`BlockWidgetKind::Image` 的 `source_range`），图片作为整体处理。桌面 fmt/clippy/test/build 全绿（43 项）。
- 2026-11-09（`8513df6`，0.1.83）：新增桌面外观设置（D62）：`settings.rs` 持久化主题（跟随系统/浅色/深色）、正文字体/等宽字体、正文字号/等宽字号到平台配置目录（`MAZHA_SETTINGS_PATH` 可覆盖）；启动时应用主题，`System` 订阅窗口 appearance；正文布局改用可调字号，行高兼容更大的等宽字号；命令面板新增外观命令，快捷键 `Cmd+=`/`Cmd+-`、`Cmd+Option+=`/`Cmd+Option+-`、`Cmd+Shift+L`、`Cmd+Option+0`，状态栏显示字号与主题。桌面 fmt/clippy/test/build 全绿（31 项）；实机验证深浅色切换、字号调整、字体循环、重置、重启持久化，以及正文 12px + 等宽 24px 极端组合。
- 2026-11-06（`063e3c4`，0.1.82）：新增 macOS `.app` 打包链路（D61）：`macos/Info.plist` + `scripts/bundle-macos.sh`，从 `assets/appicon.png` 生成完整 `AppIcon.icns`，写入 Bundle ID/显示名/版本，复制 release 二进制，默认 ad-hoc 签名或指定 Developer ID，输出 `码札.app` 与 zip；新增 `notarize-macos.sh` 处理公证 + 装订。验证 Info.plist、`codesign --verify`、Finder 文件图标、`.app` 启动。
- 2026-11-06（`16b993b`，0.1.81）：新增 `platforms/desktop/assets/appicon.png`；启动时把嵌入二进制的 PNG 解码为 `NSImage`，经 `NSApplication.setApplicationIconImage` 设置 macOS Dock 图标；新增 macOS 专用 `objc2` 依赖。桌面 fmt/clippy/test/build 全绿（24 项）；实机确认启动正常、图标解码与 `setApplicationIconImage` 调用成功。
- 2026-11-05（`ed540ca`，0.1.80）：活动标签/侧栏当前项改浅灰 `selected_bg`（文本选区仍蓝）；大纲跳转按目标视觉行顶边置顶并按内容总高 clamp；标签增加 `✕` 关闭按钮（单标签隐藏），新增 `close_tab`；文件信息/行数/光标/快捷键移入窗口底部 24px 全宽状态栏。桌面 fmt/clippy/test/build 全绿（24 项），workspace fmt/clippy/test 全绿（507）；实机核对浅灰选中、章节 8 置顶、双标签关闭与底部状态栏。
- 2026-11-03（`f64a678`，0.1.79）：标签/侧栏切换改为 `sync_current_tab` + `activate_tab`，用 move 保留各标签撤销历史并正确装载正文/光标；侧栏支持 4px 分隔条拖动缩放（160–420px），长文件名 `truncate()` 末尾省略号；桌面 24 / workspace 507 全绿，实机验证双文档切换与拖拽。
- 2026-11-03（`ee4b3cd`，0.1.78）：修复软换行下 `row_tops` 按逻辑行建表导致块重叠/首行乱码/图片与标题错位；超宽表格改为列宽线性压缩 + 单元格软换行（行高/命中/选区/光标按多视觉行），不再回退 `|` 流式；长文件名侧栏单行裁切，避免越界到正文伪装成表头乱码。桌面 fmt/clippy/test/build、workspace fmt/clippy/test 全绿（507 + 24）。
- 2026-11-02（并入本次提交）：补齐内联 HTML 标签解析（`<br>` 剥离、结束标签限制在当前行内区间、坏 hex 颜色防 panic）+ `mazha-md` 9 / core 1 条单测；修复遗留 clippy（`strip_prefix`/`range_plus_one`/`uninlined_format_args`/`collapsible_if`/未读 `viewport`）与桌面 rustfmt；workspace 507 / 桌面 24 全绿。
- 2026-10-28（选区）：`07fd742 feat(desktop): 文本选区（拖拽/Shift+点选/全选/双击选词/三击选行）+ 蓝色选区高亮`（桌面 24/24）。
