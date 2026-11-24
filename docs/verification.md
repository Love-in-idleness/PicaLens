# 0.0.1 桌面技术验证

当前验收目标为 Plasma 6 Wayland。用原生 Wayland 源窗和 XWayland 源窗各做一轮，独立 X11 会话不纳入验收。启动前确认当前桌面为 KDE、会话类型为 Wayland、KWin 启用了 OpenGL 合成，且 XWayland 可用。每轮记录 KWin、XWayland、Portal 后端、PipeWire 和 OBS 的版本，以及显示器分辨率、刷新率和缩放比例。若桌面提供 100% 和 150% 缩放，两种设置分别记录。

可以用 `qdbus6 org.kde.KWin /KWin supportInformation` 记录 KWin 的合成后端和输出信息。

在仓库目录构建后，从不同终端启动所测源窗、观察窗和探针。命令以原生 Wayland 并排为例。切换源窗模式、布局或缩放时使用单独的日志目录。

```sh
mkdir -p /tmp/picalens-verification/plasma6-wayland-tiled-100
target/debug/picalens-test-window 2>&1 | tee /tmp/picalens-verification/plasma6-wayland-tiled-100/source.log
target/debug/picalens-test-window --sentinel 2>&1 | tee /tmp/picalens-verification/plasma6-wayland-tiled-100/sentinel.log
target/debug/picalens-probe --desktop-only 2>&1 | tee /tmp/picalens-verification/plasma6-wayland-tiled-100/portal.log
```

用 `target/debug/picalens-test-window --xwayland` 启动 `PicaLens Source Window (XWayland)`。如果当前终端设置了 `GDK_BACKEND=wayland`，在这条命令前加 `env -u GDK_BACKEND`。两个模式的源窗默认窗口几何均为 800×600，显示每秒变化的计数、剧情和九个按钮。

用 `event=mapped backend=GdkWaylandDisplay fixture=wayland` 核对原生 Wayland。XWayland 轮需要 `xdpyinfo` 检查服务器的 XWAYLAND 扩展，日志应为 `event=mapped backend=GdkX11Display fixture=xwayland`。若日志为 `fixture=x11-unverified`，该次仍待确认服务器类型。`event=layout` 记录 GDK 表面尺寸，可能包含客户端阴影。用 `Open test popup` 打开独立的临时窗口。误投观察窗 `PicaLens Input Sentinel` 运行在独立进程中，记录收到的输入，可另加 `--xwayland` 测试另一类观察窗。

`--desktop-only` 记录会话环境、Portal 接口版本和后端声明的能力。记录捕获、授权和输入结果时，以选窗后的会话日志及各窗口收到的事件为准。

## 实验一 · OBS 捕获与遮挡

1. 在 OBS 添加支持系统选择窗口的 PipeWire 捕获源，并在选择器选中测试源窗。先让窗口可见 15 秒，查看预览或录屏中的计数和背景变化。
2. 用另一扇窗口完全盖住源窗，保持 OBS 输出可观察。再观察 15 秒，记录预览中的计数是否持续变化及是否中途停顿。恢复可见，记录是否恢复出帧。
3. 分别将源窗口最小化、将源窗口留在另一个工作区，每种情况观察 15 秒，再恢复源窗。分别记录结果和恢复行为。
4. 对计划支持的并排、全屏显示布局分别保留证据。在每个布局中至少记录一段完整的计数变化。需要观察全屏遮挡时，可用 OBS 录屏，结束后再查看对应片段。

随后关闭 OBS 捕获并运行默认探针，单独验证 PicaLens 使用同一源窗的捕获会话。

```sh
target/debug/picalens-probe 2>&1 | tee /tmp/picalens-verification/plasma6-wayland-tiled-100/capture.log
```

记录 Portal 授予的源类型、流尺寸、协商格式、缓冲区类型、画面是否持续更新和停止原因。同步记录 `wayland_parent_exported`，用于判断探针是否能向系统选择器传递 GTK 窗口标识。OBS 的结果和探针的结果分别填写。

## 实验二 · 输入命中

```sh
target/debug/picalens-probe --experiment 2>&1 | tee /tmp/picalens-verification/plasma6-wayland-tiled-100/input.log
```

在 Portal 选择同一源窗，记录窗口流以及实际授予的指针和键盘权限。在探针日志中核对窗口流的 `mapping_id` 与 EIS 绝对坐标区域是否能对应。区域缺失、连接失败或授权不足都记录为阻塞，保留可观察的捕获结果。

分别测试以下布局，切换布局后重新核对源窗、观察窗与探针的位置。

- **并排**。源窗口完整可见，探针在一旁。观察窗放在同一显示器，避开源窗口。
- **全屏覆盖**。将探针置于源窗口所在的显示器，用 F11 全屏。源窗保持已打开并被完全盖住。观察窗保持运行，可通过窗口管理器把它置于源窗下方，单独检查坐标和焦点误投。

交互采样只在隔离的实验桌面上用测试窗运行。F8 或窗口标题栏的 Arm sample 在五秒内启用一次采样，可尝试一次主键点击、一次离散滚动或按 F9 发送一次空格键。每次采样后等至少一秒，探针会在这段时间记录返回到自身的事件。下一次采样前将焦点切回探针，再重新启用采样。F11 切换全屏并解除启用。Esc 先解除启用，再按退出捕获。坐标映射不明确或画面过期时，探针拒绝启用输入。有裁剪的画面要求 EIS 区域尺寸和完整流尺寸完全一致，坐标再按裁剪偏移映射。

每轮在下列操作前记下时间，观察探针和接收窗口的日志。源窗、弹窗和观察窗分别以 `source`、`popup`、`sentinel` 为日志前缀，使用 `monotonic_us` 对齐记录。布置窗口时产生的物理输入与试验时产生的转发输入分开记录。

| 操作 | 核对项 |
| --- | --- |
| 按从左到右、从上到下的顺序，各点一次九个按钮 | 源窗的 `event=hit target=` 恰好依次记录 1–9，剧情跟随每次命中更新。观察窗没有对应输入 |
| 在画面内滚动一次，下一次采样按 F9 | 如果源窗收到对应的滚轮及空格键事件，记录对应日志。如果观察窗或探针收到转发事件，记录实际接收方并判为误投 |
| 调整视图宽高比，在黑边内点击和滚动 | 源窗与观察窗均没有收到对应的转发事件 |
| 打开测试弹窗，记录状态后关闭弹窗 | 当前探针无法可靠读取源窗弹窗状态。只在隔离桌面上明确测试弹窗路由时启用一次采样，记录收到事件的窗口 |
| 启用采样后将焦点切给观察窗，再恢复探针焦点 | 检查启用状态是否已解除，以及原窗和观察窗有无收到转发事件 |
| 关闭源窗、取消选窗或在桌面撤销共享 | 记录探针停止原因，检查指针和键盘状态是否释放，确认重新选窗可开始新会话 |
| 在选窗期间和捕获期间，从另一终端执行 `target/debug/picalens-probe toggle` | 分别检查选窗是否结束、捕获是否停止。空闲时执行同一命令会启动只读选窗 |

组合会话先选择输入设备和窗口源，再启动会话。若后端仅开放 D-Bus `Notify*`，把绝对指针定位作为后续的独立会话实验。已连接 EIS 的会话只经 EIS 发事件。操作系统把事件投给哪个窗口，以接收侧日志为准。 [RemoteDesktop Portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)

## 结果表

### Plasma 6 Wayland 验收

| 源窗 | OBS 与探针捕获 | 并排输入 | 全屏覆盖输入 |
| --- | --- | --- | --- |
| 原生 Wayland | 嵌套环境已验证选窗，出帧阻塞于 KWin 合成 | 组合会话未启动流，待测 | 组合会话未启动流，待测 |
| XWayland | 在 KWin 6.3.6 的 XWayland 24.1.6 上已启动源窗，KDE 选择器同时列出两种源窗。出帧仍受 OpenGL 合成限制 | 待测 | 待测 |

配对选窗截图、XWAYLAND 扩展和两个源窗的日志存放在云工作区的 `/workspace/scratch/picalens-lab/evidence-xwayland/`。

### 先前实验记录

先前两轮桌面实验使用原生 Wayland 源窗。Plasma 使用 KWin 6.3.6、KDE Portal 6.3.5、XDG Desktop Portal 1.20.3、PipeWire 1.4.2 和 OBS 30.2.3.1-3。嵌套后端因缺少 DRM 设备使用 QPainter。GNOME 50 的历史结果保留供比较。该轮使用 Shell/Mutter 50.1、GNOME Portal 50.0、XDG Desktop Portal 1.21.1、PipeWire 1.6.2 和 OBS 32.1.0，Mutter Development Kit 提供 1280×800 虚拟输出，缩放为 100%。

日志、截图与 GNOME OBS 录屏存放在云工作区的 `/workspace/scratch/picalens-lab/evidence-plasma/` 和 `/workspace/scratch/picalens-gnome-lab/evidence-gnome/`。

| 观测项 | Plasma 6 | GNOME 50 |
| --- | --- | --- |
| Portal 声明的源类型和输入设备 | ScreenCast v5（源类型 `0x3`），RemoteDesktop v2（输入设备 `0x7`），GlobalShortcuts v1 | ScreenCast v5（`0x7`），RemoteDesktop v2（`0x7`），GlobalShortcuts v1 |
| OBS 可见时持续更新 | 选中源窗后 KWin 报 `OpenGL compositing is required for screencasting`，未出帧 | 录屏 t=25.5–40.5 s，源窗计数 172→187 |
| OBS 完全遮挡时持续更新 | 阻塞于可见基线 | OBS 最大化覆盖源窗，六次间隔约 3 秒的预览采样计数为 88、92、95、99、102、105 |
| OBS 最小化与恢复 | 阻塞于可见基线 | 最小化后约 15 秒内预览计数 189→205，恢复后仍出帧 |
| OBS 切工作区与恢复 | 阻塞于可见基线 | 源窗留在另一工作区，录屏 t=124.5–139.5 s 的计数 271→286，移回后仍出帧 |
| 探针单独捕获 | GTK 父窗口标识已导出，选择器列出并可选中原生源窗。KWin 因合成限制未启动流 | 实际授予 Window。BGRA、1280×800、MemFd、stride 5120、裁剪 850×650，隐藏光标。探针全屏覆盖源窗时，约 15 秒内计数继续更新 |
| 组合会话实际授权 | 到达 `RemoteDesktop.Start`。界面只提供屏幕和输入设备，尝试启动 `X11-0` 输出流时也遇到合成限制 | 实际授予 Window、Pointer 和 Keyboard，窗口流与带相同 `mapping_id` 的 1280×800 EIS 绝对坐标区域匹配 |
| 并排输入 | 阻塞于组合会话启动 | 1–9 全部按顺序命中源窗，滚轮到达源窗，黑边点击被拦截。F9 发出的空格键回到探针，键盘验收失败 |
| 全屏覆盖输入 | 阻塞于组合会话启动 | 源窗被覆盖时，点击画面中心第 5 个按钮，转发的按下与松开均回到探针，源窗无命中。判定此布局失败，未继续其余点位和按键 |
| 弹窗、切焦点、误投观察 | 阻塞于组合会话启动 | 已验证切焦点解除启用、回到探针的空格不会再次启用采样。观察窗在已采样的时段未收到误发；探针收到误投已使验收失败。Portal 路径下的弹窗投递未测 |
| 停止原因与清理 | 选窗期间 `toggle` 关闭选择器和探针。手动关闭选择器后正常退出。活动流清理受合成限制 | Esc 和 `toggle` 都结束活动流。关闭源窗时已收到节点移除，探针退出。随后重新授权可获得新会话 |

KDE Portal 6.3.5 的 `RemoteDesktop.Start` 在启用画面共享时从输出或工作区创建流。这个版本的组合会话限制与本次 KWin 合成环境限制是两项独立结果。 [KDE Portal 6.3.5 源码](https://github.com/KDE/xdg-desktop-portal-kde/blob/v6.3.5/src/remotedesktop.cpp#L240)

KDE Portal 6.7.6 源码的 `RemoteDesktop` 启动路径只有虚拟输出、已有输出和工作区流三种分支。当前 Plasma 运行结果来自上表的 6.3.x 环境。 [KDE Portal 6.7.6 源码](https://github.com/KDE/xdg-desktop-portal-kde/blob/v6.7.6/src/remotedesktop.cpp#L182)

## 组件测试

- 在 Weston 的原生 Wayland 会话中，已检查 800×600 源窗，收到 1–9 按钮点击、键盘与滚轮日志，测试弹窗能独立打开并接收焦点。误投观察窗能单独启动并记录输入。
- 在嵌套 KWin 6.3.6 中，两类源窗并行启动并持续计数，日志分别确认 `fixture=wayland` 和 `fixture=xwayland`。直接操作 XWayland 源窗时，第 5 个按钮、滚轮和 A 键均收到事件。探针转发仍待实测。独立 X11 环境的 `--xwayland` 启动检查已验证会拒绝。
- 在隔离的 PipeWire 测试中，已验证 MemPtr、MemFd、裁剪和源节点消失。OBS 和桌面 Portal 组合会话的结果见上表。

使用当前 Codex 云端工作区已有的构建环境时，先加载其工具链和开发库，再运行 Cargo。当前构建使用 Rust 1.100.0。

```sh
source /workspace/.picalens-env/activate.sh
export BINDGEN_EXTRA_CLANG_ARGS="-resource-dir=$PICALENS_SYSROOT/usr/lib/llvm-19/lib/clang/19"
cargo build --locked
```

## 推进条件

- 0.0.1 在 Plasma 6 Wayland 上至少需要一条窗口流在目标布局中持续出帧，并分别记录原生 Wayland 和 XWayland 源窗在并排和全屏布局下的实验结果。
- 只有两种源窗均通过全屏九点、黑边、弹窗和切焦点检查，才认为 Plasma 提供可交互的放大。0.1 还要在 100% 和 150% 缩放下复测，切焦点和关闭窗口各试十轮。任何一轮误投都暂停交互发布。
- 如其中一种路径尚未通过，继续验证 KWin 集成，已通过捕获的组合可提供只读技术预览。

## 合成器集成验证入口

在 Plasma 6 Wayland 会话中，从 `picalens` 或 `picalens toggle` 进入合成器交互模式。KWin 负责选窗、等比绘制和输入映射。原生 Wayland 与 XWayland 均已在嵌套 KWin 6.3.6 的 QPainter 后端通过 100% 和 150% 缩放的输入对照。DRM/OpenGL 后端与 Portal 窗口流分别验收。 [Plasma 构建与验收](kwin-experiment.md)

Mutter 50.1 对窗口流的 EIS 绝对坐标做表面到舞台的坐标变换，然后向 Clutter 虚拟设备发送移动和按键。GNOME 50.1 实机类型绑定检查确认 Shell 可获取窗口演员、查询位置下的演员，并反算演员坐标。`Meta.WaylandPointer` 未从该版本的 GObject Introspection 暴露，设置 Wayland 指针焦点的函数位于 Mutter 内部。当前源码审查及实测尚未确认 Shell 扩展能将点击投给被覆盖的源面，不能据此开启该路径。 [窗口流坐标变换](https://github.com/GNOME/mutter/blob/50.1/src/backends/meta-screen-cast-window-stream.c#L321) · [EIS 事件提交](https://github.com/GNOME/mutter/blob/50.1/src/backends/meta-eis-client.c#L503) · [Wayland 指针焦点](https://github.com/GNOME/mutter/blob/50.1/src/wayland/meta-wayland-pointer.c#L1096)

## 0.0.4 菜单与缩放验收

- GNOME 51 实机，1920×1200、150% 缩放。桌面启动器的 Toggle 动作打开系统选窗，《村正》窗口流的裁剪区域为 1545×912，使用线性 DMA-BUF。新版全屏预览映射为 1280×800 逻辑像素，持续出帧后按 Esc 回到菜单。
- 隔离的 GNOME 51 会话，1920×1200、150% 缩放。用与《村正》窗口流等尺寸的四色角标测试纹理验证截图。等比适应截图保留四角和上下黑边，裁剪铺满截图按预期裁剪两侧。两张 PNG 均为 1920×1200。
- GNOME 51 的组合会话分别出现过仅授予键盘与指针、仅授予带 `mapping_id` 的窗口流两种结果。前者无法映射到目标窗口，后者无法启用探针的单次输入。100% 隔离会话和 150% 实机均未通过组合验收，已打开窗口继续使用只读预览。
- 隔离的 KWin 6.7.5 会话，1280×800。新后端公布 `fit`、`stretch`、`fill`，裁剪铺满把 800×500 源矩形映射到全屏。测试源窗记录了中心第 5 个按钮命中和 A 键按下及释放。
- Gamescope 3.16.24 通过 PicaLens 界面从带中文和空格的路径启动 Wine 测试程序。游戏窗口实际映射，接收鼠标点击和键盘输入。首发三语目录通过一致性检查，中文和日文界面完成运行检查，三个语言设置分别通过已安装二进制的 CLI 文案检查。

## 0.0.5 GNOME 原窗口交互

GNOME Shell 51.0 和 Mutter 51.0 的隔离会话使用 1920×1200 输出。PicaLens 选择原生 Wayland 测试窗，Shell 直接变换原窗口的演员，Mutter 负责鼠标命中。100% 和 150% 缩放均通过九点点击和滚轮检查，等比模式的黑边点击没有送给源窗。Esc 恢复原窗，快捷键命令 `picalens toggle` 可取消选窗和结束放大。扩展禁用后，同一个客户端回退到 Portal 选窗。其他 D-Bus 连接请求开始放大收到 `AccessDenied`。

在 150% 会话中，使用独立游戏副本和 Wine 前缀启动 `Muramasa_chs.exe`。隔离的 PipeWire 空音频输出满足游戏的 DirectSound 初始化。受测游戏通过 Wine Wayland 出窗后，在交互全屏中用鼠标打开「设定画面」并回到标题。随后强制结束测试客户端，扩展收到客户端断开事件并恢复窗口和 Shell 顶栏，游戏进程继续运行。

Ubuntu 27.04 的 `.deb` 包含 GNOME 扩展。首次安装到用户目录时，Shell 在重新登录时扫描并加载新扩展。已在隔离会话验收加载后的行为，当前桌面会话保留到用户自行重新登录。

## 0.0.6 GNOME XWayland 原窗口交互

- 在隔离的 GNOME Shell 51.0 会话使用 Ubuntu Mutter `51.0-1ubuntu3` 和 1920×1200 输出。仅放开扩展的 XWayland 选窗限制时，边缘点击丢失。补丁为放大中的窗口反算坐标，再沿用 Mutter 原有的 XWayland 缓冲区坐标换算。未加载补丁的 Shell 仍只在交互选窗中列出原生 Wayland 窗口。 [Mutter 51 XWayland 坐标实现](https://gitlab.gnome.org/GNOME/mutter/-/blob/51.0/src/wayland/meta-xwayland-surface.c)
- 100% 和 150% 缩放下，XWayland 测试窗均按顺序命中 1–9，接收滚轮和普通按键。黑边点击未送到源窗，Esc 结束放大。所打包的库在版本匹配时由测试 Shell 加载，模拟版本不符时启动器使用系统库。
- 同一补丁会话同时列出原生 Wayland 和 XWayland 测试窗。选中原生 Wayland 测试窗后通过中心点击、滚轮和 Esc 恢复检查。
- 在 150% 缩放下，用独立游戏副本和前缀启动 `Muramasa_chs.exe`。Wine 日志确认游戏加载了 `winex11.drv`，游戏同时出现在 XWayland 根窗口树和交互选窗中。放大后使用鼠标进入「设定画面」并返回标题。按 Esc 结束放大后返回 PicaLens，游戏进程仍在运行。

## 0.0.7 快捷键直接放大活动窗口

- 在隔离的 GNOME 51 和 KWin 6.7.5 会话通过桌面绑定发送 Ctrl+Super+Z。原生 Wayland 与 XWayland 测试窗都能直接放大，命中中心按钮，再按同一组合键恢复。
- GNOME 中用界面的「选择窗口」打开选择器并放大源窗。PicaLens 自己获得焦点时，快捷键提示没有支持的活动窗口。GNOME 和 KWin 均拒绝未持有 PicaLens 应用名称的 D-Bus 连接发起直接放大。
