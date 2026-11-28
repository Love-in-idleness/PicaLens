# PicaLens 技术验证手册与实测记录

本文档包含两部分：
1. **实测结果记录**：按特性与桌面汇总当前已完成和待验证的实测结论。
2. **通用 Portal / EIS 验证手册**：用于复现 0.0.1 阶段开始的 OBS 遮挡捕获、PipeWire 单窗流与 RemoteDesktop/EIS 输入命中实验。

---

## 一、实测结果记录

### 渲染显卡与 Anime4K

- **环境**：Ubuntu 26.10 虚拟机、KWin 6.7.5（DRM + VirGL 虚拟渲染节点 `/dev/dri/renderD128`，底层运行于宿主机 llvmpipe）。
- **交互验收**：在 `1280×768 @ 100%` 与 `1280×960 @ 150%` 下，FSR 1、SGSR、Anime4K 与原生 Wayland / XWayland 的全部 12 轮均通过九点点击、滚轮、A 键与退出恢复；100% 缩放的 6 轮同时通过黑边隔离。
  > 注：初次 Anime4K 原生 Wayland 试验因虚拟机 Libinput 计时过慢告警导致前两次点击漏投，拉开点击间隔复测后依序命中 1–9。
- **画质一致性对照**：在 Mesa 上使用固定 `16×12 → 32×24` RGB 测试图，对比 PicaLens Anime4K 与 `mpv 0.41.0`（相同 Anime4K v4.0.1 上游着色器链），两者方向与尺寸完全一致，8-bit RGB 各通道最大差值为 `1`。
- **Portal 预览与设备持久化**：
  - 在 KDE Portal 选窗授权原生 Wayland 窗口后，PipeWire 持续提供 `800×600` BGRx 单窗帧，Anime4K 在显式选中的 `renderD128` 上正常出帧且点击预览不会误投原窗。
  - 在预设中先后保存「自动」与 PCI 设备 `0000:00:01.0`，KWin 日志确认 Anime4K 使用对应 `renderD128`。
- **异常与兼容验证**：
  - 指定不存在的设备或放大时移走虚拟渲染节点均返回 `render_gpu_missing`；强制结束渲染进程返回 `render_worker_failed`；三轮退出后源窗持续运行，节点恢复后可重新枚举。
  - 切回旧版 KWin 后端时，新应用对 Anime4K 全屏报告 `interactive_gpu_backend`；移除预设中的显卡键并切回 FSR 1 后，旧版后端可从原入口正常绘制。
  - 单个 `.deb` 包已在 Debian 13 与 Ubuntu 26.10 隔离环境安装，分别自动匹配 KWin 6.3.6 与 6.7.5 后端。
- **独立渲染进程耗时基准**（VirGL / llvmpipe 软件回读，预热 1 帧后统计 3 帧）：

| 滤镜 | 720p → 1080p | 1080p → 2160p |
| --- | --- | --- |
| FSR 1 | 157.8 ms（6.10 fps） | 637.4 ms（1.60 fps） |
| SGSR | 112.5 ms（8.98 fps） | 393.5 ms（2.55 fps） |
| Anime4K | 551.8 ms（1.81 fps） | 1102.9 ms（0.91 fps） |

- **待验收**：真实双 GPU 交替选卡、拔卡恢复与物理显卡性能。

---

### FSR 1 与 SGSR 交互全屏

各组测试均确认画面经所选滤镜渲染（截图对比覆盖线性与两种滤镜差异），并依序验证九点点击、滚轮、A 键及留边隔离：

| 测试会话 | 原生 Wayland | XWayland |
| --- | --- | --- |
| Plasma 6（KWin 6.7.5，VirGL，1280×768 @ 100%） | FSR 1、SGSR 均通过 | FSR 1、SGSR 均通过 |
| niri 26.04（嵌套 OpenGL ES，1280×768 @ 100%） | FSR 1、SGSR 均通过 | FSR 1、SGSR 均通过 |
| Hyprland 0.53.3（VirGL，640×480 @ 100%） | FSR 1、SGSR 均通过 | FSR 1、SGSR 均通过 |

补充验证：Plasma 另在 `1920×1080 @ 150%` 通过 SGSR + XWayland 交互复测；Hyprland 另确认原窗打开测试弹窗时自动结束放大且焦点留在弹窗。详见 [Plasma 记录](kwin-experiment.md) · [niri 记录](niri-experiment.md) · [Hyprland 记录](hyprland-experiment.md)。

---

### niri 与 Hyprland 单窗放大

环境：Ubuntu 26.04 虚拟机（QEMU virtio GPU + VirGL），合成器经虚拟 DRM 输出，PicaLens 消费 Portal 授权的 PipeWire 节点（日志与截图存放在 `/workspace/scratch/picalens-lab/desktop-vm/`）。

| 会话 | 原生 Wayland 源窗 | XWayland 源窗 | 退出与恢复 |
| --- | --- | --- | --- |
| **niri 26.04**（GNOME Portal 50） | 持续接收 BGRx DMA-BUF 帧，FSR 1 启用；源窗由 `538×786` 缩放至 `590×470` 后继续出帧 | 持续接收 `540×420` DMA-BUF 帧，FSR 1 启用，F11 切换预览模式 | `Super+F8` 停止预览；关闭源窗自动结束 Portal 会话 |
| **Hyprland 0.53.3**（Hyprland Portal 1.3.11） | 持续接收 `560×440` BGRx DMA-BUF 帧，SGSR 启用 | 持续接收 `560×440` BGRx DMA-BUF 帧，SGSR 启用 | `picalens toggle` 结束预览并重新选窗；选中整屏在打开 PipeWire 流前被拒绝 |

> **兼容性说明**：旧版 Hyprland ScreenCast 私有接口 v3 错移了流结果的 `source_type`（[v1.3.11 源码](https://github.com/hyprwm/xdg-desktop-portal-hyprland/blob/v1.3.11/src/portals/Screencopy.cpp)），新版已修正为标准编号（[v1.4.0 源码](https://github.com/hyprwm/xdg-desktop-portal-hyprland/blob/v1.4.0/src/portals/Screencopy.cpp)）。PicaLens 在识别到旧版后端时会按旧编号解析窗口流。

---

### niri 26.04 原窗口交互

带补丁的 niri 嵌套运行于 QEMU/VirGL 的 niri Wayland 会话，从 `picalens toggle` 选窗：
- 原生 Wayland 与 XWayland（`:1`）源窗均依序收到 1–9 点击、滚轮与 A 键按下/释放。
- 黑边点击/滚轮未送达源窗，观察窗零误投。
- `Esc`、切走焦点与选窗期间再次 `toggle` 均验证通过（日志见 `/workspace/scratch/picalens-lab/desktop-vm/`）。详见 [niri 构建与试用](niri-experiment.md)。

---

### Hyprland 0.53.3 原窗口交互

在 QEMU/VirGL 的 Hyprland `0.53.3+ds-4` 会话中加载后台插件，分别通过界面 **「选择窗口」** 与 `toggle` 启动（100% 输出缩放）：
- 原生 Wayland 与 XWayland 源窗均依序收到 1–9 点击、双向滚轮与 A 键。
- 黑边输入未送达源窗，观察窗零误投。
- `Esc`、再次 `toggle`、关闭源窗与切换焦点均正常结束放大。详见 [Hyprland 构建与试用](hyprland-experiment.md)。

---

### DDE Wayland 的 Treeland 0.12.1

- **环境**：基于 GitHub 上游源码构建的 Treeland 0.12.1 与 dde-wlroots 0.20.2.1，在 Ubuntu 26.10 虚拟机中直接启动合成器（VirGL、OpenGL ES ≥ 3.1、`640×480 @ 100%`）。完整 DDM 登录流程与物理显卡性能待验收。详见 [Treeland 后端与使用](treeland-experiment.md)。
- **输入验收**：原生 Wayland 与 XWayland（`:0`，经 `xdpyinfo -queryExtensions` 确认 `XWAYLAND` 扩展）在 FSR 1 与 SGSR 下均通过九点点击、双向滚轮、A 键按下/释放与黑边隔离；关闭正在放大的原生 Wayland 窗口后，可继续选择并放大 XWayland 窗口。
- **滤镜复测**：XWayland 源窗依次运行 FSR 1 → SGSR → 线性 → FSR 1，静态区域逐像素对比确认三种滤镜输出各不相同。

---

### COSMIC、Sway 与 Denial

交互验收前提：同一会话同时授予所选窗口的流、指针与键盘权限，且窗口流 `mapping_id` 可唯一映射到 EIS 绝对坐标区域。验证顺序为先 `picalens-probe --desktop-only` 再 `--experiment`。

| 会话 | 验证结果 | 交互放大 |
| --- | --- | --- |
| **COSMIC Epoch 1.11.0** | 单窗流设置 `mapping_id: None`，合成器按输出声明 EIS 区域 · [窗口流源码](https://github.com/pop-os/xdg-desktop-portal-cosmic/blob/f448ddfb296bf7bb23517b24b2f532559a334a1c/src/screencast.rs#L302-L326) · [EIS 源码](https://github.com/pop-os/cosmic-comp/blob/8cb1802caada438bb5cc7670dfabdaca5fad0502/src/libei.rs#L29-L48) | 暂不支持 |
| **Sway 1.12**（wlr Portal 0.9.0） | Ubuntu 26.10 嵌套会话实测：ScreenCast v5 声明 `0x3`，两类源窗均可取得 Window PipeWire 流并持续输出 BGRx 帧；但缺少 `org.freedesktop.portal.RemoteDesktop`，公开虚拟指针协议无目标窗口参数 · [wlr.portal](https://github.com/emersion/xdg-desktop-portal-wlr/blob/v0.9.0/wlr.portal) · [Sway 源码](https://github.com/swaywm/sway/blob/1.12/sway/input/input-manager.c#L398-L431) | 暂不支持 |
| **Denial 0.6.10** | Debian 13 QEMU/VirGL 官方 amd64 包实测：Debian 自带 wlr Portal 0.7.1 仅声明屏幕源 `0x1`，原版 0.9.0 声明 `0x3`；两轮均缺少 RemoteDesktop。GTK 4.18 下图形探针与应用启动崩溃，单窗出帧未验收 · [发布包](https://github.com/denialwm/denial/releases/tag/v0.6.10) · [Portal 路由](https://github.com/denialwm/denial/blob/v0.6.10/packaging/arch/denial-portals.conf) · [libeis 计划](https://github.com/denialwm/denial/blob/v0.6.10/docs/libeis_support_plan.md) | 暂不支持 |

> Sway 会话已额外验证：`picalens toggle` 正确提示当前桌面不支持交互放大；从 **「选择窗口」** 使用显式只读预设可正常绘制首帧。

---

### 原版桌面交互复测

通过虚拟输出试验（将测试窗口临时移至新建虚拟输出，从实体输出全屏预览操作）复测各原版桌面；需同时验证两类窗口的输入命中、焦点隔离与原窗恢复后，才进入滤镜完整验收。

| 原版会话 | 复测结果 |
| --- | --- |
| **GNOME Shell / Mutter 51.0** | Portal 同时授予虚拟输出流、鼠标与键盘，`mapping_id` 与 EIS `800×600` 区域匹配；PipeWire 协商完成后连接 EIS，两类原窗各命中第 5 按钮，但点击会将焦点从全屏预览切往虚拟输出原窗；回收输出后 XWayland 窗口回到实体输出且位置由 `(240, 116)` 变为 `(240, 100)` · [虚拟输出接口](https://github.com/GNOME/mutter/blob/51.0/data/dbus-interfaces/org.gnome.Mutter.ScreenCast.xml#L158-L199) |
| **Sway 1.12** | QEMU/VirGL DRM 会话可创建/回收虚拟输出并移入窗口；临时座席可向 XWayland 窗送达点击、滚轮与按键，但原生 GTK 4.24 测试窗未绑定新 `wl_seat`、未收到输入 · [虚拟输入实现](https://github.com/swaywm/sway/blob/1.12/sway/input/input-manager.c#L398-L448) |
| **niri 26.04** | ScreenCast v5 声明 `0x7`，RemoteDesktop v2 设备类型为 `0x0`，探针在申请指针前停止。原版 IPC 支持移窗，虚拟输出与 RemoteDesktop 仍在上游推进 · [RemoteDesktop PR](https://github.com/niri-wm/niri/pull/1966) · [虚拟输出讨论](https://github.com/niri-wm/niri/discussions/714) |
| **DDE Treeland 0.12.1** | 上游源码构建的 Treeland + dde-wlroots 0.20.2.1 + DDE Portal 6.2.1 在 QEMU/VirGL DRM 会话中两类测试窗正常映射；ScreenCast v5 声明 `0x3`，RemoteDesktop v2 设备类型为 `0x0`（D-Bus 实测 Wayland 后端未导出 RemoteDesktop）。完整 DDM 登录待验收 · [dde.portal](https://github.com/linuxdeepin/xdg-desktop-portal-dde/blob/6.2.1/misc/dde.portal) · [接口注册](https://github.com/linuxdeepin/xdg-desktop-portal-dde/blob/6.2.1/src/wayland/portalwaylandcontext.cpp) |
| **COSMIC Epoch 1.11.0** | 嵌套会话同时授予单窗流、鼠标与键盘且两类单窗流持续出帧，但 `mapping_id` 为空，探针拒绝注入输入；发布版缺少可单独承载原窗的软件虚拟输出 · [单窗流实现](https://github.com/pop-os/xdg-desktop-portal-cosmic/blob/f448ddfb296bf7bb23517b24b2f532559a334a1c/src/screencast.rs#L302-L326) · [虚拟输出讨论](https://github.com/pop-os/cosmic-comp/issues/2878) |
| **Denial 0.6.10** | Debian 13 QEMU/VirGL 会话下 wlr Portal 0.9.0 声明窗口源但仍缺 RemoteDesktop；`weston-simple-shm` 与 XWayland 测试窗可映射，原生 GTK 4.18 测试窗与探针启动崩溃 · [发布包](https://github.com/denialwm/denial/releases/tag/v0.6.10) |

> 在缺少兼容后端的 GNOME 与 niri 会话中，从 **「选择窗口」** 启动线性等比全屏均已确认正确提示「当前桌面暂不支持交互放大」；如需使用 ScreenCast 采集路径，应显式选择窗口化等只读预设。

---

### 历史里程碑验收摘要（0.0.4 – 0.0.7）

- **0.0.4 菜单、缩放与多语言**：
  - GNOME 51 实机（`1920×1200 @ 150%`）：《村正》窗口流裁剪区域 `1545×912`（线性 DMA-BUF），全屏预览映射 `1280×800` 逻辑像素，持续出帧后 `Esc` 返回菜单；隔离会话验证等比适应与裁剪铺满截图（均为 `1920×1200` PNG）。GNOME 51 组合会话在 100% 隔离与 150% 实机均未通过输入验收，只读预览正常。
  - KWin 6.7.5 隔离会话（`1280×800`）：新后端公布 `fit` / `stretch` / `fill`，裁剪铺满映射 `800×500` 源矩形至全屏，中心按钮与 A 键命中通过。
  - Gamescope `3.16.24`：通过 PicaLens 界面从含中文和空格的路径启动 Wine 测试程序，窗口正常映射并响应鼠键。中/英/日三语目录一致性与 CLI 文案检查通过。
- **0.0.5 GNOME 原生 Wayland 交互**：
  - GNOME Shell / Mutter `51.0` 隔离会话（`1920×1200`，100% 与 150% 缩放）：Shell 直接变换窗口演员、Mutter 处理命中，通过九点点击、滚轮、黑边隔离、`Esc` 恢复与 `picalens toggle`；扩展禁用后自动回退 Portal；未授权 D-Bus 调用返回 `AccessDenied`。
  - 150% 缩放下通过 Wine Wayland 运行《村正》（`Muramasa_chs.exe`，配合隔离 PipeWire 空音频输出），放大期间可正常点击进入「设定画面」并返回标题；强制结束客户端后扩展自动恢复窗口与顶栏，游戏进程不受影响。
- **0.0.6 GNOME XWayland 交互**：
  - Ubuntu Mutter `51.0-1ubuntu3` 隔离会话（`1920×1200`，100% 与 150% 缩放）：加载反算坐标补丁后（[Mutter 源码](https://gitlab.gnome.org/GNOME/mutter/-/blob/51.0/src/wayland/meta-xwayland-surface.c)），XWayland 测试窗通过九点点击、滚轮、普通按键、黑边隔离与 `Esc` 恢复；版本不符时自动回退系统库；同会话兼容原生 Wayland 选窗；150% 缩放下 `winex11.drv` 运行《村正》复测通过。
- **0.0.7 快捷键直接放大活动窗口**：
  - GNOME 51 与 KWin 6.7.5 隔离会话通过 `Ctrl+Super+Z` 均可直接放大原生 Wayland 与 XWayland 活动窗口、命中中心按钮并再次按键恢复；PicaLens 自身聚焦时正确提示无支持窗口；未持有应用名的 D-Bus 调用被拒绝。

---

### 早期基线对比（Plasma 6.3.6 vs GNOME 50.1）

早期原生 Wayland 对比实验记录（证据目录：`/workspace/scratch/picalens-lab/evidence-plasma/`、`/workspace/scratch/picalens-gnome-lab/evidence-gnome/`、`/workspace/scratch/picalens-lab/evidence-xwayland/`）：
- **Plasma 6**：KWin 6.3.6（嵌套无 DRM，QPainter）、KDE Portal 6.3.5、xdg-desktop-portal 1.20.3、PipeWire 1.4.2、OBS 30.2.3.1-3、XWayland 24.1.6。
- **GNOME 50**：Shell/Mutter 50.1（Mutter DevKit `1280×800 @ 100%`）、GNOME Portal 50.0、xdg-desktop-portal 1.21.1、PipeWire 1.6.2、OBS 32.1.0。

| 观测项 | Plasma 6（6.3.6 QPainter） | GNOME 50（50.1） |
| --- | --- | --- |
| Portal 能力声明 | ScreenCast v5（`0x3`）、RemoteDesktop v2（`0x7`）、GlobalShortcuts v1 | ScreenCast v5（`0x7`）、RemoteDesktop v2（`0x7`）、GlobalShortcuts v1 |
| OBS 可见 / 遮挡 / 最小化 / 切工作区 | 均阻塞于 KWin `OpenGL compositing is required for screencasting` | 可见（172→187）、完全遮挡（88→105）、最小化（189→205）、切工作区（271→286）均持续出帧 |
| 探针单独捕获 | 父窗口导出正常，选择器同时列出两类源窗；受合成后端限制未出帧 | 授予 Window（BGRA `1280×800` MemFd，stride 5120，裁剪 `850×650`）；全屏覆盖时持续更新 |
| 组合会话授权 | `RemoteDesktop.Start` 仅提供屏幕/输出流，不提供单窗流（[KDE Portal 6.3.5](https://github.com/KDE/xdg-desktop-portal-kde/blob/v6.3.5/src/remotedesktop.cpp#L240) / [6.7.6](https://github.com/KDE/xdg-desktop-portal-kde/blob/v6.7.6/src/remotedesktop.cpp#L182) 源码均如此） | 同时授予 Window、Pointer、Keyboard，且 `mapping_id` 与 `1280×800` EIS 区域匹配 |
| 并排输入 | 阻塞于组合会话启动 | 1–9 点击与滚轮命中源窗、黑边拦截成功；但 F9 空格键回到探针（键盘失败） |
| 全屏覆盖输入 | 阻塞于组合会话启动 | 点击第 5 按钮事件回到探针、源窗未命中（全屏布局失败） |
| 停止与清理 | 选窗期 `toggle` 与关闭选择器正常退出 | `Esc`、`toggle` 及关闭源窗均正常清理并可重新授权 |

> **关键结论**：通用 Portal/EIS 路径在 Plasma（组合会话不支持单窗流）与 GNOME（全屏覆盖命中/焦点回到探针，且 `Meta.WaylandPointer` 未向 GI 暴露，见 [window-stream](https://github.com/GNOME/mutter/blob/50.1/src/backends/meta-screen-cast-window-stream.c#L321)、[eis-client](https://github.com/GNOME/mutter/blob/50.1/src/backends/meta-eis-client.c#L503)、[wayland-pointer](https://github.com/GNOME/mutter/blob/50.1/src/wayland/meta-wayland-pointer.c#L1096)）均无法直接实现可靠的全屏覆盖交互，由此确立了**合成器原生后端负责交互 + Portal 负责只读预览**的技术路线。

**组件级隔离测试**：
- **Weston**：验证 `800×600` 源窗 1–9 点击、键盘、滚轮、独立弹窗与误投观察窗。
- **嵌套 KWin 6.3.6**：确认原生 Wayland 与 XWayland 源窗并行计数，且独立 X11 下 `--xwayland` 启动会被正确拒绝。
- **隔离 PipeWire 测试**：验证 MemPtr、MemFd、裁剪元数据与源节点消失处理。

---

## 二、通用 Portal / EIS 验证手册

### 1. 环境准备与测试进程启动

验收时需分别对 **原生 Wayland** 与 **XWayland** 源窗各执行一轮（不测试独立 X11 会话），记录合成器、XWayland、Portal、PipeWire、OBS 版本及输出分辨率/刷新率/缩放（100% 与 150% 分开记录）。Plasma 可用 `qdbus6 org.kde.KWin /KWin supportInformation` 导出合成与输出信息。

在不同终端分别启动源窗、观察窗和探针（切换协议、布局或缩放时请使用独立日志目录）：

```sh
mkdir -p /tmp/picalens-verification/plasma6-wayland-tiled-100
target/debug/picalens-test-window 2>&1 | tee /tmp/picalens-verification/plasma6-wayland-tiled-100/source.log
target/debug/picalens-test-window --sentinel 2>&1 | tee /tmp/picalens-verification/plasma6-wayland-tiled-100/sentinel.log
target/debug/picalens-probe --desktop-only 2>&1 | tee /tmp/picalens-verification/plasma6-wayland-tiled-100/portal.log
```

- **XWayland 源窗**：运行 `env -u GDK_BACKEND target/debug/picalens-test-window --xwayland`。
- **协议核对**：原生轮核对 `event=mapped backend=GdkWaylandDisplay fixture=wayland`；XWayland 轮需安装 `xdpyinfo` 并核对 `backend=GdkX11Display fixture=xwayland`（若出现 `x11-unverified` 需手动确认服务器类型）。
- **辅助窗口**：点击 `Open test popup` 打开临时弹窗；观察窗 `PicaLens Input Sentinel` 独立运行，可加 `--xwayland` 测试 XWayland 观察窗。
- **云端 Codex 构建环境**（Rust 1.100.0）：
  ```sh
  source /workspace/.picalens-env/activate.sh
  export BINDGEN_EXTRA_CLANG_ARGS="-resource-dir=$PICALENS_SYSROOT/usr/lib/llvm-19/lib/clang/19"
  cargo build --locked
  ```

### 2. 实验一：OBS 遮挡捕获与探针单窗流

1. 在 OBS 中添加 PipeWire 窗口捕获并选中源窗，依次观察各 15 秒：**完全可见** → **被其他窗口完全遮挡** → **最小化** → **切至其他工作区**，并分别记录恢复可见后是否继续出帧（并排与全屏布局分别留存录屏证据）。
2. 关闭 OBS，运行默认捕获探针：
   ```sh
   target/debug/picalens-probe 2>&1 | tee /tmp/picalens-verification/plasma6-wayland-tiled-100/capture.log
   ```
3. 记录授予的源类型、流尺寸、格式、缓冲区类型、`wayland_parent_exported`、持续出帧情况与停止原因。

### 3. 实验二：RemoteDesktop / EIS 输入命中

```sh
target/debug/picalens-probe --experiment 2>&1 | tee /tmp/picalens-verification/plasma6-wayland-tiled-100/input.log
```

1. 核对授予的窗口流、指针/键盘权限，以及窗口流 `mapping_id` 与 EIS 绝对坐标区域的对应关系（缺失或授权不足直接记录为阻塞）。
2. 分别在 **并排布局** 与 **全屏覆盖布局（F11）** 下测试。在隔离桌面中按 `F8`（或点击 `Arm sample`）启用 5 秒单次采样（可发送一次主键点击、一次滚轮或按 `F9` 发空格），每次采样后等待 ≥ 1 秒观察日志；`Esc` 先解除启用再退出。
3. 使用 `source` / `popup` / `sentinel` 前缀与 `monotonic_us` 对齐核对下表（详见 [RemoteDesktop Portal 规范](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)）：

| 操作 | 核对标准 |
| --- | --- |
| 依序点击九宫格 1–9 | 源窗 `event=hit target=` 依次记录 1–9，观察窗零误投 |
| 画面内滚轮 + `F9` 空格键 | 源窗收到滚轮与空格；若观察窗或探针收到转发事件即判误投 |
| 黑边点击与滚动 | 源窗与观察窗均无转发输入 |
| 打开测试弹窗后采样 | 仅在隔离桌面显式测试弹窗路由时采样，记录实际接收窗口 |
| 启用采样后切焦点至观察窗 | 确认采样自动解除，原窗与观察窗无误发 |
| 关闭源窗 / 取消选窗 / 撤销共享 | 记录停止原因，确认按键/指针状态释放并可重新开始新会话 |
| 选窗期 / 捕获期运行 `picalens-probe toggle` | 分别终止选窗或捕获；空闲时执行启动只读选窗 |

### 4. 交互发布准入标准

- 原生 Wayland 与 XWayland 两种源窗均需通过全屏九点、黑边隔离、弹窗与切焦点检查，并在 100% / 150% 缩放下各完成 10 轮切焦点与关闭窗口复测。
- **零误投原则**：任意一轮出现输入误发至其他窗口，立即暂停该桌面组合的交互发布，仅保留只读技术预览。
