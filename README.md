# 瞬觅 ShunSeek（Rust 版）

> v0.1.2 界面重绘（设计来自 LeiBox 组件库）：搜索框光束游走描边（聚焦时一束彩虹光沿边框绕行）、分类栏改玻璃标签栏（选中为亮色磨砂胶囊）、窗口与色块加玻璃高光质感、弹窗玻璃化。功能与 v0.1.1 一致。
> v0.1.1 修复：拼音搜索打分切片 panic（搜不到结果的主因之一）、索引换入改内存直传（存缓存失败不再丢索引）、数据源逐个隔离并在状态栏显示来源计数、MFT 读不到时分层兜底遍历；界面按深色 Fluent 重绘。Release 以免安装版为主（绿色 zip / 单个 exe）。

不依赖 Everything 的 Windows 全盘搜索工具，**Rust 重写版**：热键呼出一个 DeskBox 式的小悬浮窗，
文件、文件夹、快捷方式、已安装软件、软件安装目录、图片、视频、音频、文档等全部可搜，
自动分类、可按分类筛选。自建索引：直读 NTFS MFT 建索引 + USN 日志增量监听。

本仓库是 .NET 版（Hahakuaizai/ShunSeek）的同功能重写，**功能口径逐项对齐 .NET 版**，
重写的目标只有三个：内存占用降到最低、索引加载更快、搜索更快。做法见下面「为什么更快更省」。

**设计第一性原则（与 .NET 版相同）：搜索速度是核心。** 引擎与 UI 完全解耦，
引擎可在任何平台单独编译和跑基准。

## 为什么更快更省（与 .NET 版对比）

| 手段 | .NET 版 | Rust 版 |
|---|---|---|
| 全路径存储 | 每条目一个 string（UTF-16） | **目录驻留**：路径拆「目录+文件名」，目录去重只存一份，条目记 4 字节目录号 |
| 名字存储 | char 池（UTF-16）+ 小写池，两份 | 原始 UTF-8 一份，搜索时折叠大小写校验，不存小写池 |
| 每条目定长开销 | 7 个并行数组 ≈29 字节 + 2 个 string 对象 | 21 字节（name_off/init_off/dir_id/cat/file_ref） |
| 索引加载 | 每次启动全量重建 | **落盘格式 = 内存布局**，mmap 零拷贝加载，实测约 0.5ms 就能搜；重建在后台进行 |
| 扫描 | 逐条目 Span.IndexOf | memchr（SIMD）在整个名字池上找「前两字节锚点」候选 + 并行分片，候选量降 1~2 个数量级 |
| 拼音 | GB2312 编码器（系统代码页） | 构建期生成的 3755 项一级字表，两次二分查找，零依赖 |

## 实测（本仓库 Bench，2026-11-25）

环境：Linux VM、Rust 1.100、**1.5 核共享 vCPU（限流）+ 6GB 内存**——数字明显慢于正常桌面机，
主要看量级与相对关系，换机请重跑。合成数据与 .NET 版 Bench 同分布同规模（200 万条目）；
注意合成数据的随机目录几乎不重复（110 万个不同目录），是目录驻留的**最不利情况**，
真实磁盘目录重复率高得多，内存还会更低。

| 指标 | .NET 版 | Rust 版 |
|---|---|---|
| 建索引（2,000,002 条） | 4020 ms | **2146 ms** |
| 索引内存 | 411 MB（粗估） | **133.2 MB（精确值）** |
| 索引加载 | 无落盘，每次重建 | **mmap ≈ 0.5 ms**（文件 133 MB） |
| 查询「报告」 | 平均 62 ms | **平均 9.6 ms** |
| 查询「合同」 | 平均 26 ms | **平均 5.4 ms** |
| 查询「report」 | 平均 127 ms | **平均 30.9 ms** |
| 查询「wzry」（拼音首字母） | 平均 101 ms | **平均 21.7 ms**，正确命中王者荣耀 |
| 查询「mp4」+ 视频分类 | 平均 32 ms | 平均 30.9 ms |
| 查询「2024」（大命中面） | 平均 117 ms | 平均 72.6 ms |

跑基准：`cargo run --release -p shunseek-bench`

## 结构

```
Cargo.toml                  workspace（release 开 LTO + 单 codegen-unit）
crates/
├─ shunseek-core/           搜索引擎（无 UI 依赖，任何平台可编译）
│  ├─ store.rs              紧凑平坦索引：目录驻留 + UTF-8 名字池 + 首字母池 + 落盘/mmap
│  ├─ search.rs             并行扫描（两字节锚点）+ 打分排序 + 取消
│  ├─ pinyin.rs             拼音首字母（GB2312 一级字边界表，pinyin_table.rs 为生成表）
│  ├─ category.rs           11 类分类器（扩展名表与 .NET 版逐项一致）
│  ├─ builder.rs            多数据源汇总建索引（MFT 父链拼路径 + 分类兜底）
│  ├─ usage.rs              打开频次（紧凑二进制落盘），排序加分规则同 .NET 版
│  └─ sources/
│     ├─ walker.rs          兜底目录遍历（全平台）
│     ├─ mft.rs             FSCTL_ENUM_USN_DATA 直读 MFT（Windows，kernel32 FFI 手写）
│     ├─ usn.rs             FSCTL_READ_USN_JOURNAL 增量监听（Windows）
│     ├─ appscan.rs         开始菜单/桌面快捷方式 + 注册表 Uninstall（Windows，winreg）
│     └─ win32.rs           最小 FFI 声明与 USN 记录解析
├─ shunseek-bench/          速度基准：合成 200 万条目 → 建索引 → 查询集 → 落盘/加载
└─ shunseek-app/            egui 界面（DeskBox 式悬浮窗重绘）
   ├─ main.rs               窗口/搜索调度/索引装配（缓存秒开 + 后台重建）
   ├─ hotkey.rs             Alt+Space 全局热键（Windows：GetAsyncKeyState 轮询）
   ├─ tray.rs               托盘图标与菜单（Windows：tray-icon）
   ├─ opener.rs             打开/定位（Windows：ShellExecuteW / explorer /select）
   └─ demo.rs               演示数据（与 .NET 版 DemoIndexSource 逐条一致）
```

## 界面（与 .NET/WPF 版功能对照）

- 700×500 无边框置顶小窗、不进任务栏、圆角；背景为深色仿 Mica（真 Mica 需 Windows DWM
  合成，egui 下用纯色+描边近似，外观布局与 WPF 版一致）。
- 搜索框（占位符「搜索应用、文件、文件夹…」+ ESC 徽标）→ 分类 chips → 结果列表 → 状态栏，
  四段布局与 WPF 版相同；chips 含「全部/应用/快捷方式/安装目录/文件夹/文档/图片/视频/音频」，
  「更多」弹出菜单含压缩包/代码/程序/其他（.NET 版此处是占位按钮，Rust 版做成了真筛选）。
- 结果列表虚拟化（只渲染可见行）；行 = 分类色块+首字、文件名（命中段高亮）、全路径、分类胶囊。
- 交互：输入防抖 30ms + 取消旧查询；↑↓ 选择、Enter 打开、Ctrl+Enter 打开所在文件夹、
  双击打开、Esc 隐藏；Alt+Space 全局呼出/隐藏；托盘菜单显示/退出；打开结果记频次影响排序。
- 启动：先 mmap 上次的 index.bin 秒开，后台用 MFT+应用扫描重建并刷新缓存
  （数据目录 %LocalAppData%\ShunSeek）；MFT 不可用时退目录遍历兜底。

## 构建与运行（Windows）

需要 Rust 工具链（rustup 装 stable 即可），不需要 Visual Studio/.NET：

```powershell
cargo build --release -p shunseek-app
.\target\release\shunseek-app.exe
```

首次启动无缓存索引，会先出演示数据/空索引，后台建好真实索引后自动换上；
MFT 直读需要管理员权限或卷读取权限，拿不到时自动退到遍历用户目录的兜底模式。

## 完成度清单（诚实版）

**已验证**
- [x] core 引擎：`cargo test` 通过（拼音单测 wzry / QQ音乐 等）；Bench 全流程实测（数字见上）
- [x] core 的 Windows 目标交叉检查：`cargo check --target x86_64-pc-windows-msvc -p shunseek-core` 通过
      （MFT/USN/注册表等 cfg(windows) 代码都参与了编译）
- [x] app 界面：`cargo check -p shunseek-app`（Linux 目标）通过
- [x] app 的 Windows 目标交叉检查：`cargo check --target x86_64-pc-windows-msvc -p shunseek-app` 通过
      （托盘/热键/ShellExecute 等 cfg(windows) 代码都参与了编译；仅剩运行时行为未验）

**未验证（需要你在 Windows 上首跑确认）**
- [ ] app 在 Windows 上的实际运行（编译已过，运行未验）
- [ ] MFT 实盘枚举与 USN 监听的运行时行为（需要真实 NTFS 卷 + 权限）
- [ ] egui 界面在本机无显示环境，未截图核对，布局按 API 语义实现

**与 .NET 版相同的未做项（不假装已实现）**
- UWP/商店应用（shell:AppsFolder）枚举、.lnk 目标解析：接入点注释已留（见 appscan.rs）
- 单实例互斥：.NET 版有，Rust 版标了 TODO（见 app main.rs 文件头）

## 已知口径差异（有意为之，留档）

- SearchHit.match_start 在 Rust 版是**字节**偏移（.NET 版是 char 偏移），UI 高亮已按字节处理。
- 频次文件格式由 JSON 改为紧凑二进制（usage.dat），规则不变；.NET 版的 usage.json 不通用。
- 拼音「乐」按 GB2312 一级字表归 l 段（QQ音乐 → qqyl）；.NET 版注释写的 qqyy 是注释笔误，
  其实现行为与 Rust 版一致。
- 冻结索引不再保留父条目下标（只在建索引拼路径时用，UI 从不读），省 4 字节/条目。
