# ShunSeek 功能对齐清单（原版 .NET HEAD e7deb97 ↔ Rust 版）

> 功能对照主表紧随本说明（下一节）；其后「设计令牌」一节由 v0.1.6 视觉对齐任务先行提取，
> 数值全部来自原版源码（ThemeManager.cs / MainWindow.xaml / LiquidPanel.cs /
> MainWindow.xaml.cs / TrayMenu.cs / UpdateWindow.xaml），单位均为 WPF
> 设备无关像素（1/96 英寸），颜色为 #AARRGGBB（省略 alpha 时为 FF 不透明）。

---

## 功能对照主表（原版 .NET HEAD e7deb97 ↔ Rust 工作树现状，app v0.1.5）

口径说明：

- 「Rust 现状」以 2026-11-25 工作树为准据实填写；标注「v0.1.6 在补」的，是已派工但尚未落盘验收的，不计为已有。
- 「用户裁定」= 与原版不同但用户亲口定过的做法，以用户为准，不算缺口。
- 视觉数值（颜色/尺寸/动画时长）不在本表重复，见文末「设计令牌」一节；本表只对功能与行为。
- 原版两个容易误读的点先钉死：①类型筛选是**单选**（全部 + 12 类点一枚即生效）；②首页 = 固定项 + 最近使用，**不是**纯固定项（纯 pins 是用户对 Rust 的新裁定）。

### 1. 窗口与显隐

| 功能 | 原版表现 | Rust 现状 |
|---|---|---|
| 全局热键呼出/隐藏 | RegisterHotKey 系统级注册；候选按序试：Ctrl+Alt+Space → Ctrl+Shift+Q → Alt+Q，首个成功即用，实际组合动态显示在托盘菜单行尾；全被占用也不崩，托盘/双击仍可呼出 | 部分有：GetAsyncKeyState 20ms 轮询（非真注册）；默认 Alt+Space，可在设置里自定义任意组合并做冲突检测；无候选回退链 |
| 显示位置 | 鼠标所在屏幕工作区内水平居中，顶边 = 工作区顶 + 高×14%，按该屏 DPI 换算 | 没有：eframe 默认落位，无按鼠标屏定位 |
| 显示/隐藏动画 | 显示：Opacity 0→1 160ms + Y 位移 10→0 220ms；隐藏：淡出 110ms + 下沉 6px，淡完才 Hide | 没有：Visible 开关硬切，无过渡 |
| 窗口层级/任务栏 | Topmost=False（可被其他软件覆盖）、ShowInTaskbar=True、780×540、最小 600×400、CanResize、WindowChrome 拉伸边 6px | 部分有：与原版正相反——AlwaysOnTop 置顶、不进任务栏、固定 700×500 不可拉伸（v0.1.6 在补：普通层级/进任务栏/可拉伸） |
| 窗口控制按钮 | 书眉右端五钮：筛选/主题/最小化/最大化-还原/关闭，Segoe 字形 + ToolTip，关闭钮悬停红底 | 没有：无边框且无任何窗口按钮（v0.1.6 在补最小化/最大化/关闭） |
| 拖动窗口 | 面板空白处按下即拖（WM_NCLBUTTONDOWN+HTCAPTION 系统接管）；双击空白 = 最大化/还原；输入框/按钮/列表区不触发拖动 | 没有（v0.1.6 在补顶部拖动区） |
| 最大化形态 | 最大化时圆角归 0、外层 Margin 归 0、投影移除、按钮字形换「还原」 | 没有：窗口不可最大化 |
| Esc 键链 | 筛选面板开着 → 先关面板；查询非空 → 清空查询；再按才隐藏窗口 | 部分有：筛选展开 → 收起；否则直接隐藏（没有「先清查询」一档） |
| 呼出时搜索框 | Show 时聚焦 + SelectAll：保留上次查询并全选，打字即覆盖 | 部分有：呼出即清空查询 + 聚焦（体感不同） |
| 点 ✕ / 系统关窗 | HEAD 行为 = 淡出后完全退出，不驻留托盘 | 用户裁定：固定为最小化到托盘（覆盖原版此项，v0.1.6 落地并撤掉原「关闭窗口后」二选一设置）；v0.1.5 现状是设置二选一 |
| 单实例 | 命名 Mutex + 命名事件：第二个实例唤醒已有窗口后自退，杜绝双索引/双热键 | 没有：main.rs 明写 TODO，可重复启动多份 |
| 失焦自动隐藏 | 无（主窗口普通层级不自动隐藏；只有托盘菜单靠全局钩子点外关闭） | 没有（v0.1.6 按用户新要求在补，属新增而非对齐） |

### 2. 搜索与键盘

| 功能 | 原版表现 | Rust 现状 |
|---|---|---|
| 搜索触发链 | 输入变化 → 取消上次查询 → 30ms 防抖 → 后台并行扫描 → 回 UI 换结果；索引刚就绪且已有查询时自动重跑一次 | 已有：30ms 防抖 + 取消标志 + 只取最新一条结果；索引分级换入后自动重搜 |
| 结果上限 | 固定取 100 条；展开折叠组时重查取 300 条 | 部分有：可调 60/100/200（默认 100）；组行展开是展示层派生，不二次取数 |
| 方向键导航 | ↑↓ 列表 ±1、首页 ±4（按磁贴列数）；←→ ±1；选中项自动滚入视图 | 部分有：↑↓ 列表 ±1、宫格 ±列数；←→ 仅宫格模式 |
| PgUp/PgDn 翻页 | ±3 步（列表 ±3 行、首页 ±3 行磁贴） | 没有 |
| Home/End | 跳到首条/末条 | 没有 |
| Ctrl+C 复制路径 | 复制选中条目完整路径到剪贴板（占用失败静默） | 没有 |
| Enter 打开 | 打开选中项（无选中退回第一条）；折叠组行 Enter = 展开 | 已有：同语义（组行 Enter 切换展开） |
| Ctrl+Enter 定位 | explorer 打开所在目录：文件用 /select 选中本体，文件夹则直接打开该文件夹 | 部分有：一律 explorer /select,"路径"（文件夹本体行为与原版不同） |
| 双击打开 | 结果行/磁贴双击打开；双击折叠组行也触发展开路径 | 已有 |
| 打开后收尾 | 记录一次使用频次并落盘 → 窗口淡出隐藏 | 部分有：记频次 + 立即隐藏（无淡出） |
| 打开失败提示 | 状态栏中间给出原因：「路径已不存在（可能被移动或删除）」或异常消息；无路径条目提示「没有记录可打开的位置」 | 没有：失败只写 stderr，UI 无提示 |
| 状态栏结果文案 | 「N 项 · X ms」（页脚居中） | 已有：「N 条结果 · X ms」（状态栏左侧） |
| 空状态文案 | 分场景：建索引中显示进度消息；索引为空「索引还在建，等它跑完再搜」；无命中「没有匹配的内容」；首页无内容「输入关键词开始搜索」 | 部分有：仅「暂无条目 / 没有找到结果」两句；建索引进度只在状态栏，不进空状态区 |
| 结果集替换动画 | 每次结果 Reset：列表 Opacity 0.55→1 淡入 140ms | 没有 |

### 3. 结果呈现

| 功能 | 原版表现 | Rust 现状 |
|---|---|---|
| 列表行内容 | 52px 行：图标 24px、名称（Serif 14）、路径小字、右端重要度点（3 档色）+ 具体类型标签，行尾发丝线（左缩进 56），整行 ToolTip = 完整路径 | 部分有：图标 28px、名称（带命中高亮）、路径小字；无重要度点、无类型标签、无整行 ToolTip（代之以悬浮详情卡，见独有节） |
| 具体类型名（FileTypeName） | 约 70 种扩展名映射到具体名（.docx→Word 文档、.py→Python 源码、.iso→光盘镜像…），认不出退回「XXX 文件」 | 没有：只有 12 类分类名 |
| 重要度三档 | High（应用/文档/文件夹）/Normal/Low（日志/缓存/临时/转储等），由扩展名表 + 噪音文件名 + 噪音目录判定；驱动行尾圆点与折叠策略 | 没有：core 只有 demoted 二值（哈希名/垃圾路径段），无三档体系 |
| 固定别针标记 | 已固定条目在列表行显示别针字形（E840，朱砂 70% alpha） | 没有（随 pins 一并缺，v0.1.6 补 pins 时需带上） |
| 结果折叠（搜索态） | 按具体类型折叠：High 永不折；同类型总数 ≥4 且超出「前 5 条保底」才折成一行「N 个<类型> · 点击展开」；全部 Low 并成一条「其他 N 项（日志/缓存/临时文件）· 点击展开」；点选折叠行即展开 | 部分有：按父目录折叠（重要段同父 ≥4、降权段 ≥2），文案「N 个同目录结果已折叠」；分组口径与保底规则与原版不同 |
| 命中高亮 | 无（MatchStart 只记不用） | Rust 独有：名字命中段朱砂高亮（拼音/子序列命中不高亮） |

### 4. 右键菜单

| 功能 | 原版表现 | Rust 现状 |
|---|---|---|
| 条目右键菜单 | 右键先选中该条，再弹：打开 / 打开所在位置 / 复制路径 / 分隔 / 添加到首页（已固定时为「从首页移除」，移除同时清掉该条使用记录防其从最近使用冒头） | 没有：全应用无右键菜单（v0.1.6 仅补「添加到首页 / 从首页移除」两项，其余仍缺） |
| 空白处右键菜单 | 添加文件夹到首页…（文件夹对话框）/ 添加文件到首页…（文件对话框，可多选）/ 分隔 / 立即刷新索引 / 重建索引 | 没有（v0.1.6 以首页「＋ 添加」格子替代前两项） |
| 菜单样式 | 全局 ContextMenu 样式与主窗同主题（深浅跟随），圆角 10、勾标记朱砂 | 没有（egui 默认菜单样式，且当前无菜单） |

### 5. 首页（空查询）

| 功能 | 原版表现 | Rust 现状 |
|---|---|---|
| 首页构成 | 固定项（全部，不设上限）+ 最近使用 24 条；固定在前、最近在后，两边重复路径只出一次 | 部分有：v0.1.5 首页 = 全部使用记录按频次排序，无 pins、无最近分区（v0.1.6 按用户裁定重做为 pins-only，见下行） |
| pins 模型 | 用户裁定（Rust）：只显示自己添加的项、强制宫格、只显示图标+名称、悬浮看详情。原版在此之上还带最近使用 | v0.1.6 在补 pins-only 宫格；「最近使用」一节按用户裁定不进首页（与原版的差异是用户点名要的） |
| pins 持久化 | %LocalAppData%\ShunSeek\pinned.json（路径 JSON 数组，损坏当空表） | 没有（v0.1.6 用数据目录 pins.txt，一行一路径） |
| pins 排序 | 固定顺序即展示顺序；新固定插入队首（最近固定的排最前）；无拖拽排序 | 没有（随 pins 缺） |
| pins 增删入口 | 条目右键 添加到首页/从首页移除；空白右键 + 系统文件/文件夹对话框添加 | 没有（v0.1.6 补：结果右键两项 + 首页＋格子选文件/文件夹 + 磁贴右键移除） |
| 最近使用排序 | TopPaths(24)：按 LastUsedUtc 降序，同刻按次数降序 | 部分有：v0.1.5 首页按次数降序、同次按最近（口径正好相反）；且无 24 条上限概念 |
| 首页标题行 | 「已固定 N · 最近使用 M」（仅有内容时显示） | 没有 |
| 首页形态 | 固定磁贴网格 170×92（图标 44 + 名称居中单行），与结果列表是两个独立 ListBox，键盘列数恒 4 | 部分有：宫格/列表随全局视图模式切换，格子 104×96（图标 40 + 名称 2 行 + 分类小字） |
| 首页与筛选 | 类型筛选只作用于搜索态，首页不受筛选影响 | 部分有：v0.1.5 空查询同样过分类掩码与范围筛选 |

### 6. 筛选

| 功能 | 原版表现 | Rust 现状 |
|---|---|---|
| 类型筛选 | 书眉漏斗按钮弹出扁平芯片面板（宽 340）：全部 + 12 类**单选**，点一枚立即生效并关面板、焦点回搜索框；按钮右上朱砂圆点表示「有筛选」；面板底部「当前：X」汇总；有筛选时才显示「重置」；按钮 ToolTip = 「按类型筛选（当前）」 | 部分有（用户裁定多选）：圆点展开胶囊**多选**（单击切换、双击 solo、选择永不为空、双击「全部」回全选）；单选被多选覆盖属能力超集，但无「重置」按钮与「当前：X」汇总行，激活指示靠圆点底色 + 计数徽标 |
| 范围筛选 | 无（原版靠索引范围本身限定，见第 10 节模式分档） | Rust 独有：预设（桌面/文档/下载/用户主目录）+ 磁盘 + 自定义文件夹多选，core 层 ScopeFilter 按目录边界匹配 |
| 筛选面板关闭 | Popup StaysOpen=False：点外即关；Esc 先关面板（见 Esc 键链） | 部分有：Esc 收起已有；点外关闭在 v0.1.5 真机失效，v0.1.6 在补（圆点再点/Esc/点外三路） |

### 7. 托盘

| 功能 | 原版表现 | Rust 现状 |
|---|---|---|
| 托盘图标与提示 | NotifyIcon，ToolTip「瞬觅 ShunSeek」；图标三级兜底：程序集 app.ico → exe 关联图标 → 系统默认图标 | 已有：tray-icon + 自绘渐变放大镜图标（.ico 已嵌入 exe），ToolTip 同文案 |
| 左键单击 | 切换主窗口显隐 | 已有：左键抬起切换（双击连发两次切换，与原版近似） |
| 右键菜单构成 | 自绘玻璃菜单（与主窗同主题）：显示搜索（行尾灰字提示当前热键）/ 立即刷新索引 / 重建索引 / 以管理员身份重启（行尾提示「全盘索引」）/ 开机自启（勾选开关）/ 退出（朱砂危险色） | 部分有：系统原生菜单 5 项——打开瞬觅 / 重新建立索引 / 设置 / 关于瞬觅 vX.Y.Z / 退出；缺 立即刷新、管理员重启、自启开关、热键提示四项；多出 设置/关于（原版无，设置在页脚齿轮菜单） |
| 托盘菜单关闭 | 自绘 Popup + GlobalClickWatcher 全局低级鼠标/键盘钩子：点外任意像素或 Esc 关闭（点击不吞、原样放行）+ 前台窗口变化 500ms 看门狗兜底 | 已有（机制不同）：原生菜单由系统托管，天然点外关闭 |
| 托盘退出可靠性 | 菜单动作直达 App.Shutdown，无唤醒链 | 部分有（v0.1.5 真机失效）：事件 handler 入队 + request_repaint 唤醒 update()，真机隐藏态下断链；v0.1.6 改专职托盘线程 + 原生消息循环 + 独立看门狗强退 |
| 首次运行气泡 | 首启弹气泡 5 秒：「瞬觅已在托盘 / Win11 可能把它收进了 ^ 折叠区，拖出来固定一下就好。{热键} 呼出搜索。」flag 文件保证只弹一次 | 没有 |
| 管理员身份重启 | 托盘与设置菜单各一入口：runas 拉起同一 exe，UAC 取消抛异常兜住、保持现状；提权后索引升级为全盘 MFT | 没有 |

### 8. 设置（原版无独立设置页，是页脚「设置」按钮的弹出菜单）

原版设置菜单全项：切换为浅色/深色主题 / —— / 立即刷新索引 / 重建索引 / 以管理员身份重启（全盘索引）/ —— / 检查开发目录改动并更新 / 更改开发目录… / —— / 退出。

| 功能 | 原版表现 | Rust 现状 |
|---|---|---|
| 设置载体 | 页脚齿轮按钮弹出 ContextMenu（即点即办，无设置窗口） | 部分有：独立设置窗口（viewport），分区：常规/外观/搜索/索引/数据/关于（功能覆盖更广，形态不同） |
| 主题切换入口 | 设置菜单一项 + 书眉主题按钮，两处 | 已有：状态栏主题按钮 + 设置窗口「外观」，同一 set_theme 入口 |
| 立即刷新索引 | 设置菜单 + 托盘 + 空白右键，三处 | 没有（无增量通道可刷，只有全量重建） |
| 重建索引 | 设置菜单 + 托盘 + 空白右键，三处 | 部分有：设置窗口 + 托盘两处（重建中文案置灰逻辑两边都有） |
| 以管理员身份重启 | 有 | 没有 |
| 检查更新/更改开发目录 | 有（DevUpdater，见第 15 节） | 没有 |
| 退出 | 设置菜单末项「退出」 | 部分有：设置窗口无退出项，退出只在托盘 |
| 开机自启 | 只在托盘菜单（勾选项）；注册表 HKCU Run 值 = `"exe路径" --tray`（静默进托盘）；IsEnabled 还校验值里是当前 exe 路径（程序挪窝算未开） | 部分有：设置窗口复选框，状态以注册表为准；但写入值只有 `"exe路径"`、无 --tray 参数，且程序本身不解析 --tray（自启会直接弹窗）；不校验路径是否为当前 exe |
| 热键设置 | 无设置项（固定候选链，见第 1 节） | Rust 独有：捕获式自定义 + 冲突检测 + 推荐候选（见独有节） |
| 关闭窗口行为 | 无设置项（✕ 行为固定） | 用户裁定：v0.1.5 的二选一设置（最小化到托盘/退出程序）将被 v0.1.6 撤销，固定最小化到托盘 |

### 9. 主题

| 功能 | 原版表现 | Rust 现状 |
|---|---|---|
| 双主题 | 深色（默认）/浅色两套完整色板（37 命名画刷 + InkFill 渐变 + TopGlow 光晕 + 主题投影），切换 = 整批换资源 + 窗口 Opacity 0.72→1 淡入 170ms；持久化 theme.txt（dark/light） | 部分有：深/浅双 Palette 全覆盖，持久化 settings.txt 的 theme 键（兼容旧 theme.txt 迁移）；切换无过渡动画 |
| 主题作用范围 | 主窗、筛选面板、右键菜单、托盘自绘菜单、更新窗全跟随（DynamicResource） | 部分有：主窗 + 设置窗口跟随；托盘是系统原生菜单不跟随 |

### 10. 索引构建与数据源

| 功能 | 原版表现 | Rust 现状 |
|---|---|---|
| 权限分档（索引模式） | 管理员 + NTFS 卷 → MFT 模式全盘直读；否则便携模式：UserProfile + 公共桌面/文档/开始菜单 + ProgramFiles + ProgramFilesX86 + ProgramData | 没有分档：一律逐盘试 MFT（普通权限下读卷多半失败）→ 兜底遍历 %USERPROFILE% → 再兜底全盘遍历；无提权路径、无便携范围定义 |
| MFT 枚举 | FSCTL_ENUM_USN_DATA，1MB 批读全卷，父引用链拼路径（≤64 层防环） | 已有：同 FSCTL、同拼路径法 |
| 软件扫描 AppScanner | 开始菜单（用户+公共）与桌面（用户+公共）的 .lnk → Shortcut（显示名去 .lnk 后缀、按名去重）、.exe → App；注册表 Uninstall（HKLM 64/32 + HKCU）：DisplayName → App（SystemComponent=1 跳过、按名去重），打开路径优先 DisplayIcon 的 exe、次 InstallLocation；InstallLocation → InstallDir 条目 | 已有：同四目录 + 同三 hive + 同跳过/取值规则（winreg 实现）；UWP/shell:AppsFolder 两边都是 TODO 未做 |
| 遍历兜底 DirectoryWalker | 迭代栈遍历、跳过目录联接防环、坏目录跳过；**噪音目录名整树跳过**（cache/caches/gpucache/code cache/logs/temp/tmp/node_modules/.git/.svn/.vs/__pycache__/shadercache 等 20 个） | 部分有：栈遍历/防环/跳坏目录都有；**无噪音目录名跳过**（core walker 无名单，app 侧也没有） |
| 便携模式排除前缀 | 13 项路径前缀整树排除：xwechat\radium、WeChat\All Users、Local\Temp、Local\Packages、Local\Microsoft\Windows、CrashDumps、Recent、ShunSeek 自家目录、Chrome/Edge 的 Cache、VS Code 的 Cache/CachedData/logs | 没有：无任何排除前缀（这是用户实测「搜 wx 全是 radium 哈希目录」的原版解法；Rust 靠搜索降权+折叠兜，索引本身照单全收） |
| 建索引去重 | MFT 枚举到的 .lnk 若与软件条目同路径，MFT 条让位跳过（防同一快捷方式出现两行） | 没有：builder 无此去重，appscan 与 MFT/浅层会同收一个 .lnk |
| 友好名 FriendlyNames | 索引内单存「显示名 ≠ 文件名」的条目名（注册表名/快捷方式名，几千条量级），显示与搜索都以友好名为准；随缓存落盘 | 部分有：数据源给的名字直接进名池（appscan 的 DisplayName 效果等价）；但 MFT/遍历条目只有文件名，无二次名解析（见第 14 节显示名） |
| 建索引进度 | 节流 300ms 推一次状态：「正在建立索引 · N 条…」 | 部分有：分级换入（应用+浅层先行、逐盘并入）+ 状态栏逐盘文案，形式不同、信息量相当 |
| 索引内存管理 | 建完/重建后强制 LOH 压缩一次并记内存诊断日志（防托管堆空洞显胖） | 不适用（Rust 无 GC）；Rust 索引为目录驻留紧凑结构（21 字节/条目量级） |

### 11. 增量索引（原版三通道：USN / FileSystemWatcher / 定时重建兜底）

| 功能 | 原版表现 | Rust 现状 |
|---|---|---|
| USN 增量监听 | 每块 NTFS 卷一个 UsnWatcher：建索引前先记各卷 NextUsn，建完从该点回放（建索引期间的变更不丢）；FSCTL_READ_USN_JOURNAL 阻塞读（BytesToWaitFor + 100ms 超时），原因掩码 = 创建/删除/改名旧名/改名新名 | 没有：core 有 usn.rs（轮询读日志的实现）但 app 未接线（已核查 main.rs 无引用）；新文件只能靠手动重建或重启进索引 |
| 增量应用节奏 | 变更进无锁队列，300ms 定时器批量应用：重冻增量表 + 原子换快照；验收线「文件建好 1 秒内可搜」 | 没有 |
| 增量表语义 DeltaIndex | 新增按 FileRef（NTFS）/路径（兜底）双键去重；删除记墓碑三类：引用号、精确路径、目录前缀（整棵子树失效）；搜索快照 = 基础 + 增量 − 墓碑 | 没有：无增量表、无墓碑、无快照合并 |
| 重建触发条件 | 墓碑 >20000 或死目录前缀 >32 → 要求重建；目录改名 → 前缀墓碑 + 强制重建（子条目路径全变、USN 不逐个通知）；目录删除 → 只记墓碑不重建；USN 日志被覆盖 → 重建；父目录未登记 → 重建 | 没有（无增量即无此机制；Rust 现靠手动重建） |
| 重建调度 | ScheduleRebuild 防抖合并：默认延迟 2s，监听溢出延迟 25s，刚建完 60s 内的抖动延迟到 60s；重建期间搜索照常用旧快照 | 部分有：手动重建期间搜索用旧索引（内存直传换入）这一点一致；自动调度没有 |
| 便携模式增量 | DirectoryUpdateWatcher（FileSystemWatcher）盯便携 roots：64KB 缓冲、只报文件名/目录名变更、排除目录的变更不上报；缓冲区溢出 → Overflow → 延迟重建 | 没有 |
| USN 故障降级 | 卷未开 USN 日志 → 状态提示「该卷只有全量索引（新文件需手动刷新）」；监听中断 → 状态栏报卷与原因 | 没有 |
| 增量状态显示 | 状态栏：「N 项 · 增量 M · HH:mm 更新」；每次增量应用后消息「增量更新 · 累计 K 次变更 · HH:mm:ss」 | 没有：状态栏只有条数与来源摘要，无增量计数与更新时间 |

### 12. 索引缓存与启动

| 功能 | 原版表现 | Rust 现状 |
|---|---|---|
| 缓存内容 | index.cache（%LocalAppData%\ShunSeek）：成品数组全量（名/首字母/**全拼**三池 + 分类 + 父/文件引用号 + 全路径 + 友好名表）+ scope 串 + 建表时间；魔数 SSIX、版本 3；先写 .tmp 再原子替换 | 部分有：index.bin 存紧凑索引（名池 + 首字母池 + 目录驻留表），魔数 SSKIDX01 v1，tmp+rename 原子换；无全拼池（随全拼功能缺）、无 scope/时间戳字段 |
| 缓存就绪路径 | 命中缓存 → mmap/读入即刻就绪可搜，同时起增量监听补漏 | 部分有：mmap 缓存顶上即可搜；但不补增量（无监听） |
| 缓存过期判定 | scope 对不上或缓存 >24h：先用着，15s 后后台重扫一遍 | 没有：启动无条件全量重建并逐级换入（结果不旧，但每次启动都全扫，无 scope 概念） |
| 缓存保存时机 | 首次建完、每次整表重建后，后台异步写，不挡 UI；空索引不写 | 已有：最终一级换入时存 index.bin；保存失败在状态栏明示（不静默） |
| 启动显隐 | 默认启动即弹窗；带 --tray/--silent（开机自启走这条）静默进托盘 | 部分有：启动即显示；无静默启动参数（自启会弹窗，见第 8 节） |

### 13. 搜索语义与打分

| 功能 | 原版表现 | Rust 现状 |
|---|---|---|
| 名字子串通道 | 小写名 Span.IndexOf 子串匹配，按核数分片并行扫；单分区命中 ≥16384 时先排序截断到 4096 防爆 | 已有：分片并行扫（memchr 锚点候选 + 验证），基础机制一致 |
| 拼音首字母通道 | 查询为纯 ASCII 字母/数字时启用：命中分 前缀 80 / 子串 50 | 已有：同 GB2312 一级字边界表（Rust 用构建期码表）；档位为 75/60（见打分行） |
| 全拼拼音通道 | 「weixin」搜到「微信」：TinyPinyin 音节表建全拼池；**仅当前两通道总命中 < limit 时才扫**；命中分 前缀 72 / 子串 45，App/Shortcut +34、InstallDir/Folder +4，短名 +8，全拼与查询全等 +30；与名字命中按条目去重（名字通道优先） | **没有**：索引无全拼池（store 只有名池 + 首字母池），搜 weixin 找不到微信 |
| 打分档位（名字通道） | 前缀 100 / 词首 70 / 子串 40；−命中位置×0.1 −名长×0.01；全名等值 +30；短名（≤6 字）+8 | 部分有：自有档位——精确 200 / 前缀 160 / 分段前缀 120 / 子串 80（段界定义含驼峰与汉字交界）；档位数值与原版不同，排序体感有差 |
| 类别加权 | App/Shortcut +38、InstallDir +10、Folder +5（压得住同名文件堆） | 部分有：App/Shortcut/InstallDir/Folder 一律 +5（权重弱得多，同名噪音更容易冒头） |
| 使用频次加分 | boost = min(次数,50)×0.2 +（7 天内用过 +2），封顶约 12 分，只动同档顺序 | 已有：usage.rs 同公式同封顶 |
| 降权 | 哈希名 −30（名长 ≥8、纯 0-9a-f 且含数字）；路径深度 >3 层起 −3/层（封顶 −15）；含 \AppData\ −6；含 \$Recycle.Bin\ −10 | 部分有：哈希名 −60（主干 ≥12 且 ≥80% hex，口径不同）、垃圾路径段 −40（\temp\ \cache\ \radium\ \xwechat\ 等段名表，可叠加）；无深度/AppData/回收站罚分；且降权结果被展示层拎到尾段折叠（原版只压分不分组） |
| 搜索取消 | CancellationToken：新输入取消旧查询，取消的查询返回空、UI 丢弃 | 已有：取消标志 + 结果只取最新一条 |
| 搜索范围限定 | 无独立范围筛选；范围 = 索引模式的覆盖面 | Rust 独有：ScopeFilter 在 core 扫描内按根目录边界过滤（见第 6 节） |

### 14. 图标与显示名

| 功能 | 原版表现 | Rust 现状 |
|---|---|---|
| 图标获取链 | ① IShellItemImageFactory 按路径取本项图标（64px）→ ② SHGetFileInfo 真路径大图标 → ③ SHGFI_USEFILEATTRIBUTES 按扩展名/文件夹取通用图标 → ④ 文件夹图标兜底；设计红线是「绝不画分类色块」 | 部分有：只有 SHGetFileInfoW 大图标（32px）一档，取不到就回退分类色块；无 ShellItem 工厂、无通用图标降级（v0.1.6 补 USEFILEATTRIBUTES 重试一档）；且 v0.1.5 真机因 worker 线程未初始化 COM 全灭（v0.1.6 在修） |
| 图标线程模型 | 2 个专职线程（不用线程池，防被建索引饿死），Normal 优先级，每线程 CoInitializeEx(STA)；队列上限 4096；同 key 并发请求合并成一次 | 部分有：单个 worker 线程；v0.1.5 未做 COM 初始化（v0.1.6 补）；同 key 只发一次请求 |
| 图标缓存 key | 文件夹/安装目录/程序/快捷方式 = 全路径；普通文件 = 扩展名（10 万个 .jpg 只取一次） | 已有：key 口径相同（dir 路径 / path: 全路径 / 扩展名） |
| 图标失败重试 | 失败只记次数不焊死空结果，<3 次可重试；满 3 次才缓存空（防启动瞬间 Shell 忙导致永久灰块） | 没有：失败即缓存空、不重试；且全局上限 256 个 key，超限后新 key 不再取图 |
| 图标显示尺寸 | 列表 24px、首页磁贴 44px（图源 64px，HighQuality 缩放） | 部分有：列表 28px、宫格 40px，但图源只有 32px |
| 显示名解析 ShellNames | 渲染期后台按路径解析并缓存：文件夹 → Shell 本地化名（Desktop 显示「桌面」）；.lnk → Shell 显示名（天然不带 .lnk）；.exe → 文件版本信息里的 FileDescription（如「微信」）；与文件名相同则不替换 | 没有：显示名 = 索引里的名字（文件名或注册表 DisplayName），文件夹不本地化、exe 不取描述、.lnk 靠 appscan 预先去后缀（MFT 来的 .lnk 仍带后缀显示） |

### 15. 开发目录更新（DevUpdater，原版自带、Rust 全缺）

| 功能 | 原版表现 | Rust 现状 |
|---|---|---|
| 更新检测 | 触发：启动 2.5s 后自动查一次 + 设置菜单手动查。比对：开发目录 src/ 下源码（.cs/.xaml/.csproj/.axaml/.manifest/.ico/.ps1/.json，排除 bin/obj）的 mtime vs 安装目录 ShunSeek.dll 的 mtime，数出更新文件数 | 没有 |
| 更新提示 | 有更新 → 页脚亮朱砂 pill「可更新」（ToolTip「N 个文件比安装目录新 · 点击编译并重启」，出现时淡入 220ms）；无更新点查 → 状态栏「安装目录已经是最新的」 | 没有 |
| 编译与替换 | dotnet publish（win-x64、自包含、ReadyToRun、非单文件）到 %LocalAppData%\ShunSeek\update-staging，超时 5 分钟；再写 apply-update.ps1 交 pwsh 7 后台执行：等本进程退出（≤90s）→ robocopy 覆盖安装目录 → 带 --updated 拉起新版 → 删暂存目录 | 没有 |
| 更新说明 | publish 前生成 pending-update.json（时间/目标目录/改动文件数/变更条目 = 上次部署以来 git log --no-merges 的提交标题去重）；新版启动时 TakePendingUpdate 读后即删，先弹主窗再弹更新说明窗（书页风、宽 480、高随内容：「已更新到最新版本」+ 时间·已应用到目录 + 变更清单；无 git 条目时退回「本次更新包含 N 个文件的改动」；Esc/关闭按钮关，弹出与关闭各记一条日志） | 没有 |
| 开发目录配置 | devroot.txt 持久化，默认 E:\project_开\ShunSeek\ShunSeek；设置菜单「更改开发目录…」用文件夹对话框改，改完即重查并提示 | 没有 |
| 定位说明 | 这是开发者自用通道（本机源码 → 本机安装目录），不是面向用户的 OTA | Rust 侧如要对齐，需另立项；优先级低于搜索/索引类缺口 |

### 16. 进程、日志与命令行

| 功能 | 原版表现 | Rust 现状 |
|---|---|---|
| 崩溃日志 | 全局三路捕获（Dispatcher/AppDomain/Task）落盘 %LocalAppData%\ShunSeek\logs\crash-*.log；UI 异常标记 Handled，不让单个异常带走进程 | 部分有：数据源/重建/搜索各线程 catch_unwind 隔离 + 空索引红字护栏；无崩溃日志文件 |
| 诊断日志 | logs\shunseek.log 记关键节点（启动参数、热键注册结果、索引就绪、托盘菜单弹出坐标）；更新另有 logs\update.log | 没有：只有 stderr 输出（真机双击启动无处查看） |
| 命令行参数 | --query <词> 预填并搜；--tray/--silent 静默启动；--theme light\|dark 临时覆盖不落盘；--dev-update 直走更新；--shot/--open-filter/--tray-menu/--update-preview 为离屏验证用 | 没有：不解析任何参数 |
| 数据目录与文件 | %LocalAppData%\ShunSeek：index.cache、usage.json、pinned.json、theme.txt、devroot.txt、pending-update.json、deployed-at.txt、tray-hint-shown、logs\ | 部分有：数据目录同名（%LocalAppData%\ShunSeek），文件为 index.bin、usage.dat（二进制）、settings.txt、（v0.1.6 起）pins.txt；无 logs 目录 |

### 17. Rust 独有（原版没有，不算缺口，勿误删）

| 功能 | 说明 |
|---|---|
| 范围筛选 | 工具栏圆点展开：预设/磁盘/自定义文件夹多选（永不为空集），core 扫描内按目录边界过滤；首页空查询也受其约束 |
| 路径模式搜索 | 查询含 \ 或 /（或 :) 时按路径片段对目录段做子串对齐匹配；尾随 \ 列出对齐到的目录本身；可与通配叠加（如 `3089\*.txt`）；`"` `<` `>` `\|` 当噪声剔除 |
| 通配符搜索 | 名字段含 * / ? 时整名/子串窗口两档匹配 |
| 子序列模糊通道 | 首轮命中 <30 条时才补扫子序列（紧凑度计分），与首轮结果天然不重叠 |
| 命中段高亮 | 列表与宫格名字按 match_start/match_len 高亮命中段（原版无高亮） |
| 列表/宫格双视图 | 工具栏按钮手动切换 + 设置里定启动默认；原版是「首页恒宫格、搜索恒列表」的固定双形态，无手动切换 |
| 悬浮详情卡 | 行/格悬停约 0.4s 出玻璃卡：全名、全路径、类型、大小（按需读 fs 元数据并缓存）、修改时间（本地时区）；比原版整行 ToolTip（只有路径）信息多 |
| 自定义热键 | 设置里捕获任意「修饰键+主键」组合：系统保留名单硬判 + RegisterHotKey 探测占用（1409）+ 只推荐检测可用的候选，一键采用；改完即时生效（轮询读共享值） |
| 独立设置窗口 | 五个分区（常规/外观/搜索/索引/数据/关于）+ 结果上限可调 + 清除索引缓存 + 清除使用记录（两步确认） |
| 索引分级换入 | 冷启动无缓存时：应用+浅层目录先建小索引顶上（几百毫秒可搜），再逐盘 MFT 并入；原版冷启动无缓存须等全量建完 |
| 空索引护栏与退出看门狗 | 索引为 0 条时状态栏红字明示；退出意图发出后 1.5s 看门狗强制结束进程（托盘退不掉的兜底） |

---

## 设计令牌（原版视觉参数全表）

### 1. 双主题色板（ThemeManager.cs，37 个命名画刷 + 2 个渐变 + 投影）

| 令牌 | 用途 | 深色（默认） | 浅色 |
|---|---|---|---|
| ShellStroke | 窗口外壳描边 | #2A343A | #C3CBC4 |
| AccentBrush | 朱砂强调（焦点条/选中标记/圆点） | #C4573F | #B2432C |
| AccentText | 强调文字（选中芯片/危险项/pill） | #D97A5F | #9C3A24 |
| TextHi | 主文字 | #EDE9E1 | #1B2320 |
| TextMid | 次文字 | #9AA5AB | #53605A |
| TextLow | 弱文字（路径/状态/副标题） | #737E84 | #6F7B74 |
| HairLine | 发丝分隔线 | #1E262B | #DDE2DC |
| Surface | 表面（搜索框/磁贴底） | #161D22 | #FFFFFF |
| SurfaceHover | 悬停浮层 | #1C242A | #F4F6F3 |
| SurfaceSel | 选中/按下浮层 | #212B32 | #EAEFE9 |
| EdgeLine | 一般描边 | #242D33 | #CCD3CC |
| EdgeStrong | 强描边（书眉竖线） | #303B42 | #B0BBB2 |
| RowHover | 结果行悬停 | #161D22 | #F2F5F1 |
| RowSel | 结果行选中底 | #212B32 | #E8EDE7 |
| RowSelStroke | 结果行选中描边 | #2F3B43 | #C8D1C8 |
| FoldFill | 折叠组行底 | #151D22 | #F0F3EF |
| IconPlate | 图标占位盘底 | #1B242A | #EDEFEA |
| ChipFill | 小芯片底（Enter 键帽） | #1A2227 | #F2F4F1 |
| MenuFill | 菜单/筛选面板底 | #141A1F | #FCFDFB |
| MenuStroke | 菜单描边 | #263036 | #D6DCD5 |
| MenuHover | 菜单项悬停 | #1E282E | #EDF1EC |
| TooltipFill | 工具提示底 | #181F24 | #FCFDFB |
| ThumbFill | 滚动条滑块 | #2B353C | #C0C8C0 |
| PillFill | 朱砂 pill 底（强调色+alpha） | #C4573F @ 12%（1F） | #B2432C @ 10%（1A） |
| PillHover | pill 悬停层 | #C4573F @ 23%（3A） | #B2432C @ 19%（30） |
| PillStroke | pill/选中磁贴描边 | #C4573F @ 35%（59） | #B2432C @ 35%（59） |
| PinMark | 首页固定标记 | #C4573F @ 70%（B3） | #B2432C @ 70%（B3） |
| CloseHover | 关闭按钮悬停底（两主题同） | #C14432 | #C14432 |
| ClosePressed | 关闭按钮按下底（两主题同） | #A63728 | #A63728 |
| CloseText | 关闭按钮悬停字色（两主题同） | #FDF7F2 | #FDF7F2 |
| ImportanceHigh | 重要度点·高 | #C4573F | #B2432C |
| ImportanceNormal | 重要度点·中 | #7C8890 | #8A948D |
| ImportanceLow | 重要度点·低 | #4A555C | #C2CAC2 |
| ShadowColor | 全部投影颜色 | #000000 | #4A5A50 |
| ShadowOpacity | 全部投影不透明度 | 0.55 | 0.28 |

渐变（LinearGradientBrush）：

| 令牌 | 定义 |
|---|---|
| InkFill（窗口底） | 方向 (0,0)→(0.45,1)；深色 #12181C →(0.55) #0E1418 → #0A0F12；浅色 #FAFBF9 →(0.55) #F2F4F1 → #E9ECE8 |
| TopGlow（顶部光晕层） | 方向 (0,0)→(0.2,1)；深色 白 @8%（14）→ 透明；浅色 白 @40%（66）→ 透明。渲染为窗口顶部高 220 的矩形层，整体 Opacity 0.5，不参与命中测试 |

主题机制：默认深色；持久化 `%LocalAppData%\ShunSeek\theme.txt`（"dark"/"light"）；切换 = 整批替换资源画刷 + 窗口淡入动画（见 §10），不做逐刷颜色动画。

### 2. 字体

| 令牌 | 字体栈 | 用在 |
|---|---|---|
| SerifFont | Noto Serif SC → 华文中宋 → SimSun | 书眉标题、搜索输入、结果名、磁贴名、空状态提示、折叠组文字、更新窗标题 |
| SansFont | Noto Sans SC → Microsoft YaHei UI | 其余全部（路径、状态、菜单、芯片、页脚） |
| IconFont | Segoe Fluent Icons → Segoe MDL2 Assets | 全部图标字形 |

常用字形码点：筛选 E71C、主题 E706、最小化 E921、最大化 E922、还原 E923、关闭 E8BB、搜索 E721、固定 E840、折叠组 E76C、设置齿轮 E713、勾 E73E、更新 E895。

### 3. 窗口与外壳

| 参数 | 值 |
|---|---|
| 默认尺寸 | 780 × 540 |
| 最小尺寸 | 600 × 400，可拉伸（ResizeMode=CanResize） |
| 拉伸边框 | WindowChrome ResizeBorderThickness = 6，CaptionHeight = 0 |
| 任务栏/置顶 | ShowInTaskbar = True，Topmost = False |
| 外层留白 | 根 Grid Margin = 12（给投影留位），背景 #01000000（近透明但可命中拖拽/拉伸） |
| 外壳面板 | LiquidPanel Radius = 12，描边 ShellStroke 1px，填充 InkFill |
| 外壳投影 | DropShadow BlurRadius 26 / ShadowDepth 6 / Direction 270°，颜色/不透明度随主题（ShadowColor/ShadowOpacity） |
| 显示位置 | 鼠标所在屏幕内水平居中；顶边 = 工作区顶 + 工作区高 × 14% |
| 最大化时 | 圆角归 0、外层 Margin 归 0、投影移除、最大化字形换还原 E923 |
| 内容裁剪 | 外壳内 Grid ClipToBounds = True（光晕与内容不溢出圆角） |

### 4. LiquidPanel（全应用圆角的唯一实现）

| 参数 | 值 |
|---|---|
| 圆角曲线 | 超椭圆（squircle）\|x\|ⁿ+\|y\|ⁿ=rⁿ，指数 n = 4（属性默认 Radius 24 / Exponent 4，各使用点显式给 Radius） |
| 采样 | 每角 14 段折线 |
| 内外角关系 | 只用一个 Radius，同心内角 = 外角 − 内缩距离（不做双半径椭圆角） |
| 描边 | 沿圆角路径的 1px Pen（StrokeThickness 默认 1），即所谓「边缘高光」 |

各使用点的 Radius：外壳 12、搜索框 12、结果行 10、磁贴 12、筛选芯片 8、折叠组行 9（普通 Border）、托盘菜单面板 13、更新窗页 14。

### 5. 书眉（标题栏，Grid.Row 0）

| 元素 | 参数 |
|---|---|
| 容器 | 高 46，Margin 左 26 / 上 14 / 右 16 |
| 标题「瞬觅」 | Serif 22，TextHi |
| 竖分隔线 | 宽 1 × 高 14，EdgeStrong，左右 Margin 各 14 |
| 副标题「SHUNSEEK · 本机索引」 | Sans 9.5，TextLow |
| 工具按钮（ToolButton） | 34 × 30，左 Margin 2（关闭按钮 8），悬停浮层圆角 8（SurfaceHover），字形 13（窗口控制字形 10），前景 TextMid → 悬停 TextHi，手型光标 |
| 筛选激活圆点 | 5 × 5 AccentBrush 椭圆，位于筛选按钮右上（Margin 上 4 / 右 3），无筛选时隐藏 |
| 关闭按钮 | 同 ToolButton，但浮层为 CloseHover / 按下 ClosePressed，悬停前景 CloseText |

### 6. 搜索框（Grid.Row 1）

| 元素 | 参数 |
|---|---|
| 面板 | 高 54，Margin 左 26 / 上 14 / 右 26，LiquidPanel Radius 12，Fill = Surface，描边 EdgeLine 1px |
| 搜索字形 E721 | 16 号，TextLow，Margin 左 17 / 右 11 |
| 输入文字 | Serif 17，TextHi，光标（Caret）= AccentBrush，无边框、Padding 0、右 Margin 12 |
| 占位文字「搜索文件、应用、文件夹…」 | Serif 16，TextLow，左偏移 1 |
| 焦点标记条 | 宽 2 的 AccentBrush 竖条，左 Margin 1、上下 Margin 10，圆角 1；聚焦淡入 150ms / 失焦淡出 200ms |
| 「Enter」键帽 | 圆角 5，ChipFill 底 + EdgeLine 1px 边，Padding 7,3，右 Margin 14；文字 Sans 9.5 TextLow |

### 7. 正文区与页脚布局

| 元素 | 参数 |
|---|---|
| 搜索框下发丝线 | 高 1，Margin 左 26 / 上 16 / 右 26，HairLine |
| 正文容器 | Margin 左 26 / 上 12 / 右 18（右侧少 8 给滚动条） |
| 首页说明行（HomeCaption） | Sans 10.5，TextLow，下 Margin 10 |
| 空状态提示 | Serif 14，TextLow，水平+垂直居中 |
| 页脚容器 | Margin 左 26 / 上 10 / 右 20 / 下 14；顶部发丝线 1px，其下内容 Margin 上 11 |
| 状态文字（左） | Sans 10，TextLow，MaxWidth 340，超长省略 |
| 结果计数（中） | Sans 10，TextLow，居中 |
| 更新 pill「可更新」 | 圆角 7，PillFill 底 + PillStroke 1px 边，内容 Margin 9,4；朱点 5×5 AccentBrush（右 Margin 6）；文字 Sans 10 AccentText；右 Margin 10；悬停层 PillHover（淡入 120 / 淡出 180）；出现时整 pill 淡入 220ms |
| 设置按钮 | 浮层圆角 7（SurfaceHover，120/180ms），内容 Margin 8,4；齿轮 E713 11 号 TextMid（右 Margin 5）+ 文字「设置」Sans 10.5 TextMid |


### 8. 搜索结果行（列表，ResultRow 样式 + DataTemplate）

| 元素 | 参数 |
|---|---|
| 行高 | 52 |
| 行面板 | LiquidPanel Radius 10；内容 Margin 左 18 / 右 12 |
| 悬停浮层 | RowHover，淡入 120ms / 淡出 180ms |
| 选中浮层 | RowSel 底 + RowSelStroke 1px 边，淡入 140ms / 淡出 180ms |
| 选中标记条 | 3 × 26 AccentBrush，圆角 1.5，左 Margin 6，与选中浮层同步淡入/淡出 |
| 行底发丝线 | 高 1，Margin 左 56 / 右 12，HairLine；悬停或选中时隐藏 |
| 图标 | 24 × 24（列宽 30），未加载时占位盘 = 圆角 6 的 IconPlate；实图 HighQuality 缩放 |
| 名称 | Serif 14，TextHi，单行省略 |
| 路径 | Sans 10，TextLow，上 Margin 2，单行省略；名/路径列 Margin 左 12 / 右 12 |
| 固定标记 | 字形 E840，9 号，PinMark 色，右 Margin 8（仅已固定项） |
| 重要度点 | 5 × 5 椭圆，右 Margin 7，颜色按重要度三档（ImportanceHigh/Normal/Low） |
| 类型标签（TypeLabel） | Sans 10，TextLow；ToolTip 显示分类名 |
| 整行 ToolTip | 条目完整路径 |

### 9. 折叠组行

| 元素 | 参数 |
|---|---|
| 容器 | 圆角 9 Border，FoldFill 底，Margin 2,4,6,4，Padding 左/右 14 |
| 字形 E76C | 9.5 号，TextLow，右 Margin 8 |
| 文字（如「N 个日志 · 点击展开」） | Serif 12，TextMid |

### 10. 首页磁贴（TileItem 样式 + DataTemplate）

| 元素 | 参数 |
|---|---|
| 磁贴尺寸 | 170 × 92，项 Margin 右 12 / 下 12（WrapPanel 横排） |
| 面板 | LiquidPanel Radius 12，Fill = Surface，描边 EdgeLine 1px |
| 图标 | 44 × 44 居中；占位盘 40 × 40、圆角 10、IconPlate；图标行高 54，上 Margin 10 |
| 名称 | Serif 12.5，TextHi，居中，Margin 左 14 / 右 14 / 下 10，单行省略；ToolTip = 完整路径 |
| 悬停 | 浮层 SurfaceHover 淡入 140 / 淡出 200ms；内容整体缩放 1.03（放大 160ms / 还原 200ms，中心原点） |
| 选中（键盘焦点） | 浮层 SurfaceSel + PillStroke 1px 边，淡入 160 / 淡出 200ms |

### 11. 筛选面板（FilterPopup）

| 元素 | 参数 |
|---|---|
| 锚定 | 贴筛选按钮下方，VerticalOffset 8，HorizontalOffset −306（右对齐按钮） |
| 面板 | 宽 340，Padding 12，圆角 12 Border，MenuFill 底 + MenuStroke 1px 边 |
| 面板投影 | DropShadow BlurRadius 22 / ShadowDepth 5 / Direction 270°，颜色/不透明度随主题 |
| 打开动画 | 变换原点 (1,0) 右上角；Opacity 0→1 用 140ms（线性）；Scale 0.96→1 用 170ms CubicEase EaseOut |
| 标题「筛选类型」 | Sans 11，TextLow，Margin 2,0,2,8 |
| 重置按钮 | 文字 Sans 10.5 AccentText，圆角 5，Padding 6,2；仅有筛选时可见，靠右 |
| 芯片网格 | UniformGrid 3 列，13 个选项（全部 + 12 类） |
| 芯片（FilterChip） | 高 30，Margin 2；Sans 11.5 TextMid；内容 Margin 左 10 / 右 8；悬停层 MenuHover 圆角 8（淡入 100 / 淡出 160ms）；选中层 PillFill + PillStroke 1px、圆角 8（淡入 140 / 淡出 160ms），选中文字变 AccentText 并在右侧显示勾 E73E（9 号 AccentText） |
| 底部汇总「当前：X」 | Sans 10，TextLow，Margin 2,8,2,0 |

### 12. 右键菜单（ContextMenu / MenuItem 全局样式）

| 元素 | 参数 |
|---|---|
| 面板 | MenuFill 底 + MenuStroke 1px 边，圆角 10，Padding 6，最小宽 184 |
| 面板投影 | DropShadow BlurRadius 20 / ShadowDepth 5 / Direction 270°，随主题 |
| 字体 | Sans 12.5，TextHi |
| 菜单项 | Padding 9,6；悬停层 MenuHover 圆角 6（淡入 100 / 淡出 140ms）；勾 E73E 10 号 AccentText，右 Margin 8 |
| 分隔线 | 高 1，Margin 9,5，HairLine |

### 13. 托盘菜单（TrayMenu.cs，自绘 Popup）

| 元素 | 参数 |
|---|---|
| 面板 | LiquidPanel Radius 13，MenuFill 底 + MenuStroke 1px 边；投影 BlurRadius 26 / ShadowDepth 5 / Direction 270°（颜色随主题，代码内未显式设不透明度，走 DropShadow 默认） |
| 内容区 | 宽 232，Margin 6（面板总宽 ≈ 246） |
| 弹出动画 | PopupAnimation.Fade（系统淡入） |
| 菜单行 | 圆角 8 Border，Padding 10,7；列布局 = 图标 20 / 文字 / 提示 |
| 行图标 | Segoe Fluent 字形 12.5，TextMid（危险项 AccentText） |
| 行文字 | Sans 12.5，TextHi（危险项「退出」AccentText） |
| 行尾提示（如热键标签） | Sans 10.5，TextLow，左 Margin 14 |
| 行悬停/按下 | 无动画即时换色：悬停 MenuHover，按下 SurfaceSel，抬起回 MenuHover |
| 分隔线 | 高 1，Margin 10,5,10,5，HairLine |

### 14. 滚动条与工具提示

| 元素 | 参数 |
|---|---|
| 滚动条 | 宽 10，轨道透明，无箭头按钮（翻页区不可见）；滑块 ThumbFill，圆角 4，Margin 3,2 |
| 工具提示 | TooltipFill 底 + EdgeLine 1px 边，圆角 7，Padding 9,5；Sans 11，TextMid |

### 15. 更新窗（UpdateWindow.xaml，「书页」风）

| 元素 | 参数 |
|---|---|
| 窗口 | 宽 480，高随内容，MaxHeight 620 |
| 页面板 | LiquidPanel Radius 14，InkFill 底；投影 BlurRadius 30 / ShadowDepth 8 / Direction 270° |
| 外层 | Grid Margin 0,0,18,18；内容 Margin 24,18,22,16 |
| 图标盘 | 30 × 30，圆角 8；更新字形 E895 13 号 |
| 标题「已更新到最新版本」 | Serif 15，TextHi |
| 副标题 | Sans 10，TextLow，上 Margin 3；其下另有 Sans 10 的「{时间} · 已应用到 {目录}」行与发丝线（上 Margin 11） |
| 关闭按钮 | 30 × 28，浮层圆角 8（SurfaceHover），字形 E8BB 10 号 |

### 16. 动画总表（时长 / 缓动）

未注明缓动的 WPF DoubleAnimation 均为线性（无 EasingFunction）；CubicEase EaseOut 只用在下面标出的三处。

| 动画 | 时长 | 缓动 / 备注 |
|---|---|---|
| 窗口显示 | Opacity 0→1 用 160ms；位移 Y 10→0 用 220ms | 位移 CubicEase EaseOut；不透明度线性 |
| 窗口隐藏 / 关闭退出 | Opacity →0 用 110ms；位移 Y →+6 用 110ms | 线性；淡完才真正 Hide/Shutdown |
| 主题切换过渡 | RootLayer Opacity 0.72→1 用 170ms | CubicEase EaseOut |
| 结果集替换 | 列表 Opacity 0.55→1 用 140ms | 线性；每次 Results Reset 都触发 |
| 筛选面板打开 | Opacity 0→1 用 140ms；Scale 0.96→1 用 170ms | 缩放 CubicEase EaseOut，原点右上角 (1,0) |
| 更新 pill 出现 | Opacity 0→1 用 220ms | 线性 |
| 工具按钮悬停层 | 入 120ms / 出 180ms | 关闭按钮、设置按钮、pill 悬停层同此节奏 |
| 磁贴悬停层 | 入 140ms / 出 200ms | 内容缩放 1.03：入 160ms / 出 200ms |
| 磁贴选中层 | 入 160ms / 出 200ms | — |
| 结果行悬停层 | 入 120ms / 出 180ms | — |
| 结果行选中层 + 标记条 | 入 140ms / 出 180ms | 两者同步 |
| 芯片悬停层 | 入 100ms / 出 160ms | 选中层：入 140ms / 出 160ms |
| 菜单项悬停层 | 入 100ms / 出 140ms | 右键菜单 |
| 搜索框焦点条 | 入 150ms / 出 200ms | — |
| 托盘菜单弹出 | 系统 PopupAnimation.Fade | 无自定义时长 |

对齐要点（给 v0.1.6）：原版动画的共同性格是「入快出慢」（出场时长普遍比入场多 40–60ms）、幅度克制（位移 ≤10px、缩放 ≤1.03、淡入起点不低于 0.55），且全部只动 Opacity / 位移 / 缩放三类属性，不做颜色动画。
