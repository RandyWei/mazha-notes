# HANDOFF — 码札 / Mazha 交接与进度总览

> 每次提交代码时**同步更新本文件**（约定，见 `AGENTS.md`）。
> 单一事实来源是 `SPEC.md`（接口契约 §6、决策 §14、变更记录 §15）；本文件只做「当前进度 / 下一步」的归总，不代替 SPEC。

- 最近更新：2026-10-27
- 当前阶段：**Phase 1（共享核心）已完成**；**Phase 2/3 已完成**（桌面只读投影 → WYSIWYG 标记显隐/命中 → IME/编辑命令/多光标 → 文档级坐标 API → 块级 widget 渲染 → 表格增删行列/代码高亮/软换行/剪贴板/视觉行移动/查找替换/公式与图表/脚注/图片本地/远程/data/协议相对渲染）；**Phase 4 桌面 MVP 主体达成**（文件管理打开/保存/新建/重命名/删除/移动 + 多标签页 D37 + 命令面板 D38 + 大纲/目录导航 D39）；**导出线**并行：HTML / PDF / DOCX / EPUB 均可用
- 仓库：`git@github.com:RandyWei/mazha-notes.git`（私有，发版后转公开）；分支 `main`
- 当前版本：**0.1.76**（细节见 SPEC §15 变更记录；本节只列归总）

## 门禁（提交前必须全绿）

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# 桌面端（独立 workspace，见 R10；触碰 platforms/* 必须单独跑）
cd platforms/desktop && cargo fmt --check && cargo clippy -- -D warnings && cargo build
```

> rustc 需 **≥ 1.92**（workspace `rust-version`，Typst 0.15 的 MSRV），工具链见 `rust-toolchain.toml`（stable）。

## crate 进度

> 详细变更逐版本记录在 `SPEC.md` §15；下表只给「能力面 + 待办」。

| crate | 状态 | 能力面（要点） | 待办 |
|-------|------|----------------|------|
| `mazha-core` | ✅ Phase 1 完成 | `Document`（rope/编辑/undo-redo/块缓存+投影缓存**双增量**维护/`apply_and_sync`）、`OffsetMap`、`Projection`（`BlockStyle`/`TextRun`/`Widget`/`RunStyle` + **块级 widget** `BlockWidget{source_range,line_range,kind}`、`BlockWidgetKind::{Code{lang,fenced},Table{align},Rule,Math,Chart{lang},Image{alt,dest}}` + `block_widget_at`/`block_widget_on_line`）、**文档级跨行坐标**（`DocPos`/`LineDisplayRange`/`source_to_doc_pos`/`doc_pos_to_source`/`line_of_source`/`selection_display_ranges`）、显示列↔字节换算（软换行用）、`commands`、`IntentMapper`、`table::edit_table`、`highlight::Highlighter`（syntect）、`find::{find_all,replace_all,replace_all_edit}`、导航（行范围/CJK 词范围）、`Workspace`（open/save/create/remove/rename/list/find）、`crdt` 桥接 | `DocId` |
| `mazha-md` | ✅ Phase 1 完成（方言全齐） | `parse`（ATX/Setext 标题、段落、分割线、围栏/缩进代码、引用、列表/任务、GFM 表格、Callout、Front-matter、**`$$…$$` 显示公式**、**`[^label]: body` 脚注定义**）、`parse_inline`（强调/粗体/删除线/行内代码/链接/图片/自动链接/wiki-link/tags/`$…$` 行内公式/**`[^label]` 脚注引用**/转义）、`serialize_source`/`to_md`（前缀穿线，逐字节往返 + 幂等）、`reparse`（脏块增量 + 裁切点安全校验） | HTML 块、惰性续行、多段落脚注（见 SPEC §9.1/§15） |
| `mazha-store` | ✅ MVP | `StorageProvider`（同步，D24）、`FileSystemProvider`、`DocPath`、`Entry`、`StoreError` | 文件监听、异步/流式（Phase 5）、云端/插件 Provider |
| `mazha-crdt` | ✅ MVP | `CrdtDoc`（`Y.Text`，字符↔字节）、insert/delete/replace/push、状态向量 + 增量更新（Yjs v1）、`apply_update`、`fork`、`snapshot`、并发收敛、落盘（`save_crdt`/`load_crdt`） | 多文档名册 |
| `mazha-export` | ✅ HTML + PDF + DOCX + EPUB + 主题/模板 | `to_html*`（完整外壳 + 默认 CSS + 主题）、`to_typst*` + `to_pdf*`（`ExportWorld` 实现 `typst::World` → `typst::compile` + `typst_pdf`；`PdfDocument{bytes,warnings,pages}`；真实文档回归 `tests/pdf_real_doc.rs`）、`to_docx*`（docx-rs 1.1 直出 OOXML）、`to_epub*`（Zip + XHTML，EPUB 3，输出确定）、`Theme`/`Template`；块/行内特性与编辑器同源（含公式：HTML `div/span.math`、Typst 字面量 `#raw`、DOCX 纯文本；含脚注：HTML `sup.fnref` + `div.footnote`、Typst `#super[…]`、DOCX 纯文本） | 字体子集（Web，Phase 7） |
| `mazha-plugin` | ⬜ 占位 | — | WASM 宿主、能力授权（Phase 5） |
| `mazha-ffi` | ⬜ 占位 | — | UniFFI 绑定（Phase 6） |
| `mazha-wasm` | ⬜ 占位 | — | Web 绑定（Phase 7） |

## 平台进度（`platforms/*`，各自独立 workspace）

| 平台 | 状态 | 已实现（要点） | 待办 |
|------|------|----------------|------|
| `desktop`（GPUI + gpui-kit 0.7） | 🟡 Phase 2/3 主体完成（Phase 4 MVP 主体达成） | 只读投影渲染 + 主题；光标/点击命中 + **行内格式局部展开**（光标在链接/粗体/斜体/删除线/行内代码内时**该组**显示原文，`inline_reveals`/`reveal_inline`，D42）+ **Cmd+点击打开链接**；行内代码用正文前景色、引用块整行浅底（各层引用都铺底 + 左侧竖线，仅最外层加上下 padding；底色限制在正文列内，D52）；自写 `Element`（连续布局 + 视觉行表 `row_starts`）+ 跨行选区；**滚动**：自持 `ScrollHandle`（`track_scroll`）+ 外层叠加 `Scrollbar`，内容高度两帧收敛、光标行滚动跟随（含 `cx.notify`）；**IME**：全 UTF-16 区间换算、组合文本按 marked 区间替换（修复拼音字母逐个上屏）；空行/纯语法行命中落行首；编辑命令（行内 Cmd+B/I/X/`、块级 Cmd+1-6/段落/引用/列表）；多光标（Alt+Click）；PDF 导出（Cmd+E）；文档级坐标 API 接入；块级 widget 装饰（代码面板**浅底+语言标签**、表格网格、分割线、**公式描边、图表弱底+标签**）；**中文斜体渲染器级合成倾斜**（macOS 中文字体无斜体字面、CoreText 也不为 CJK 合成，故 `oblique.rs` 取 CT 字形轮廓做 12° 剪切自绘，CJK 段以透明 run 占位保留排版；D43）；**行内代码可见底色**（`code_bg` = 前景 alpha 0.08，经 `TextRun.background_color` 逐视觉行绘制）；**代码块浅底 + 左右 padding**（`code_block_bg`，缩进 +1.5em、文本 +12px、面板右缘与正文右边距对齐，D51）；**Tab/Shift+Tab 缩进/反缩进**（D44：覆盖 Root 的焦点切换绑定，行选区整体缩进，多光标重映射）；**列表/任务回车续行**（D45：走核心 `IntentMapper`，新项图标即时显示，空项再回车退出列表，嵌套保留缩进，引用内列表也续行；修复空 `ListItem` 无样式导致图标不显示）；**Typora 式就地显源码**（光标进入围栏代码块/独占图片行 → 整行就地显出原文，`BlockGeom::raw_src` 走 1:1 字符↔字节映射，移开复原）；**图片真实渲染**（D53/D54/D55：核心保留 `dest`，独占行产出 `BlockWidgetKind::Image`；桌面按文档目录解析本地路径、`http(s)` 与协议相对 `//host/...` 走 GPUI `Resource::Uri` + `ImageAssetLoader` 下载缓存，`data:image/...` 内联 base64/百分号编码走 `Image::from_bytes`，按正文宽等比缩放且不放大小图，行高随图片；光标进入显示 `![alt](dest)`，缺失/行内图片仍占位；启动时安装 `reqwest` HTTP 客户端）；表格增删行列 + **表格观感**（表头加粗/底色、分隔行收起、单元格留白，D46）+ **按内容列宽/竖向网格线/列对齐 + 单元格 Tab 导航**（D47）+ **表格内左右方向键跳过隐藏标记/竖线/填充**（D48）+ **表格光标视觉命中按最近格距离选择**（D49）+ **从表格外横移第一下即进入单元格**（D50）；代码高亮；软换行；剪贴板 Cmd+C/X/V；视觉行上下移动；查找/替换 Cmd+F；行内公式 `RunStyle::Math`（斜体+主题色）；脚注标签 `RunStyle::Footnote`（主题色）；**Phase 4：打开 Cmd+O / 保存 Cmd+S / 另存为 Cmd+Shift+S、文件树侧栏（新建/重命名 ✎/删除 ✕/移动）、脏状态 `*`；多标签页（D37，Cmd+T/W/Shift+W/1..9/0）；命令面板（D38，Cmd+P，命令表+过滤+执行）；大纲/目录导航（D39，侧栏标题列表+点击跳转，Cmd+Shift+O）** | 文档内搜索、主题与专注模式（Phase 4 余项）；文件树目录层级/移动拖拽
| `ios` / `android` / `harmony` / `web` | ⬜ 仅 README | — | Phase 6 / 8 / 7 |

> 桌面端入口：`cargo run -- [path/to/file.md]`。渲染效果可可视化验证：`swift` 查窗口号（CGWindowListCopyWindowInfo）→ `screencapture -x -o -l<窗口号>` 截图 → PIL 逐像素核对底色/字形（本机屏幕录制权限已可用）。交互可用 `CGEvent.postToPid` 合成按键（无需 `osascript` 置前；置前脚本偶发 -1719，重试即可），已验证 Tab/Shift+Tab/回车等链路。

## 测试现状

| crate | 数量 |
|-------|------|
| `mazha-core` | 198 |
| `mazha-md` | 149 |
| `mazha-export` | 121 单测 + 3 集成（`tests/pdf_real_doc.rs`） |
| `mazha-crdt` | 11 |
| 合计 | **497**（其余为占位 crate 的版本号冒烟测试） |
| `platforms/desktop`（独立 workspace） | 24（不计入上面的 497） |

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

1. **Phase 4 余项**：桌面**主题 / 专注模式 / 打字机模式 / 字体字号**（文件管理、多标签页 D37、命令面板 D38、大纲 D39 均已落地）；**全工作区搜索（`Cmd+Shift+F`）列为低优先规划（D40，暂不排期）**；**当前文档查找/替换** `Cmd+F` 已于 0.1.49 实现；文件树可择机深化为目录层级/拖拽移动。
2. **中文斜体合成仍不完美**（D43 遗留）：当前 `paint_path` 自绘倾斜只保证"倾斜"本身，字符间距/笔画抗锯齿与原生 italic 字面仍有差异（透明占位 run 的 advance 与自绘轮廓起点可能相差不足 1px），且每次 paint 逐字取轮廓；后续可考虑：轮廓路径缓存到 `BlockGeom`（只在 shape 变化时重算）、按 run 基线与 advance 校正、或改用 Metal 自定义着色器做剪切变换。观感需实机对比 Typora 后决定优先级。
3. **渲染层深化**（R4）：数学排版（`ratex-*`）与图表 SVG（`mermaid-rs-renderer`）替换块级占位面板；脚注的点击跳转/自动编号/文末归集；软换行**横向滚动**（D33 已知限制）。
4. §9.1 剩余非 MVP 项：HTML 块、惰性续行、多段落脚注定义。
5. `mazha-export`：字体子集（Web，Phase 7）；`mazha-crdt`：多文档名册；`mazha-store`：文件监听（Phase 5）。

## 提交历史（近期，共 100 条；本次提交为最新一条，下次同步时补入哈希）

```
<本次提交：feat(desktop): 内联 data URI 与协议相对图片（0.1.76，D55）>
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

> 更早提交见 `git log`；历史累计 = 99（含本次）。
