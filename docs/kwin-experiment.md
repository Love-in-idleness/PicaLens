# Plasma 6（KWin）Wayland 交互后台

PicaLens 在 Plasma 6 Wayland 会话中通过 KWin Effect 插件实现交互全屏放大：
- **PicaLens 主程序**：负责触发选窗、取消、结束会话以及管理独立 GPU 渲染进程。
- **KWin 后台插件**：负责窗口捕获、画面缩放/滤镜绘制以及鼠标与键盘输入坐标逆变换。

支持的目标版本为 **Debian 13（KWin 6.3.6）** 与 **Ubuntu 26.10（KWin 6.7.5）**，覆盖原生 Wayland 与 XWayland 两种源窗口，源应用全程保持运行。Portal 通用路径验证另见 [docs/verification.md](verification.md)。

---

## 1. 构建环境与编译

后台插件直接调用 KWin 私有 C++ 接口，**必须使用与目标会话 KWin 二进制来自同一发行版构建的开发包与运行库**。

### 依赖要求
- **通用依赖**：C++23、CMake ≥ 3.24、Ninja、Qt ≥ 6.8、ECM、`libepoxy` 及匹配的 `kwin-dev` / `kwin-wayland`。
- **KWin 6.7.5 额外依赖**：`libdrm` 与 Vulkan 开发包。
- **参考环境（Debian 13）**：`kwin-dev` / `kwin-wayland` `4:6.3.6-1`、Qt 6.8.2、KDE Frameworks 6.13.0。
- **发布验收环境**：具备 DRM 与 OpenGL 合成的 Wayland 会话，且已启用 XWayland。

### 编译与本地暂存

```sh
cmake -S kwin -B build/kwin -G Ninja -DKDE_INSTALL_PLUGINDIR=plugins
cmake --build build/kwin
cmake --install build/kwin --prefix "$PWD/build/kwin-stage"
cargo build --locked
```

暂存插件位于 `build/kwin-stage/plugins/kwin/effects/plugins/picalens.so`。

---

## 2. 启动、调试与恢复

### 加载本地构建的插件

在启动独立测试会话的 KWin 之前，将暂存目录加入 `QT_PLUGIN_PATH`：

```sh
export QT_PLUGIN_PATH="$PWD/build/kwin-stage/plugins${QT_PLUGIN_PATH:+:$QT_PLUGIN_PATH}"
```

> 每次重新编译插件后，都需要重启测试会话以加载新库。

### 启动放大与基本操作

```sh
target/debug/picalens
# 聚焦目标窗口后，也可直接通过快捷键或以下命令切换放大
target/debug/picalens toggle
```

- **界面选窗**：点击 **Choose Window…**，应用会自动加载后台插件；光标变为十字后点击任意已打开的普通窗口。
- **快捷键直放大**：在交互全屏预设下，快捷键或 `picalens toggle` 直接放大当前活动窗口，再按一次恢复。
- **取消与退出**：选窗时按 `Esc` 或再次执行 `picalens toggle` 取消；放大期间执行 `picalens toggle` 退出并恢复原窗。
- **日志确认**：在 KWin 日志中确认 `picalens started protocol=` 匹配本轮源协议，出现 `picalens first frame drawn` 后再进行输入测试。
- **输入行为说明**：后台仅转发已完成的点击、滚轮与正常焦点下的键盘输入；指针移出画面、明显拖动或退出时会自动取消未完成的点击。XWayland 应用报告的客户区可能包含透明客户端阴影。

### 渲染显卡与 Anime4K

在 PicaLens 编辑放大预设时可指定 **「渲染显卡」**：
- **FSR 1 / SGSR 选择「自动」**：沿用 KWin 自身的 OpenGL 上下文。
- **FSR 1 / SGSR 指定具体显卡**：由独立进程 `/usr/lib/picalens/picalens-render` 在所选设备上运行滤镜（进入模式时自动启动）。
- **Anime4K 选择「自动」**：优先使用当前会话的渲染设备。
- **设备持久化**：当渲染节点编号变动时，预设按 PCI 或平台设备路径重新匹配原设备。

调试时可在 KWin 日志查看 `picalens render GPU`（记录实际 DRM 渲染节点与驱动型号），等待 `picalens first frame drawn` 后验证输入。多卡或异常测试应覆盖：不关闭源窗切换第二张卡、在活动期间移除所选显卡或强制结束渲染进程，并确认停止原因及源窗可继续操作。

### D-Bus 状态检查与卸载

应用 D-Bus 连接断开时，KWin 后台会自动结束当前放大会话。开发调试常用命令：

```sh
qdbus6 org.kde.KWin /Effects org.kde.kwin.Effects.isEffectSupported picalens
qdbus6 org.kde.KWin /Effects org.kde.kwin.Effects.isEffectLoaded picalens
qdbus6 org.kde.KWin /Effects org.kde.kwin.Effects.unloadEffect picalens
```

---

## 3. 测试窗口与验收清单

在不同终端启动原生 Wayland 源窗、XWayland 源窗及误投观察窗（观察窗摆放在输出另一侧并全程保持运行）：

```sh
cargo build --locked
target/debug/picalens-test-window
target/debug/picalens-test-window --xwayland
target/debug/picalens-test-window --sentinel
```

测试前记录 `kwin_wayland --version`、`qdbus6 org.kde.KWin /KWin supportInformation`，并核对源窗日志中的 `event=mapped`、`event=layout` 与 `fixture=wayland` / `fixture=xwayland`（XWayland 轮需用 `xdpyinfo` 确认 `XWAYLAND` 扩展）。

### 验收项（原生 Wayland 与 XWayland 各测一轮）

| 测试项 | 通过标准 |
| --- | --- |
| 合成器选窗与持续更新 | 比例正确，源窗进程与剧情状态连续；保持放大 ≥ 15 秒计数持续变化，光标位置准确（源窗隐藏光标时显示桌面主题箭头） |
| 九宫格点击、滚轮与按键 | 按从左到右、从上到下点击 1–9，源窗 `event=hit target=` 依次记录 1–9；滚轮与普通按键直达源窗，观察窗零误投 |
| 黑边隔离 | 在黑边点击和滚动，源窗与观察窗均无输入 |
| 按住主键退出 | 按住任意按钮不松手，用快捷键退出并移出画面松开；源窗与观察窗均不触发点击，重新选窗后点击正常 |
| 弹窗与切焦点 | 打开测试弹窗或切走焦点时放大自动退出，核对 `picalens stopped:` 原因，确认无卡键或误投 |
| 快捷键直放大 | 聚焦源窗后按 `Ctrl+Super+Z` 直接放大，再按一次恢复，焦点保留在源窗 |
| 取消、卸载与异常恢复 | `Esc` / `toggle` 可取消选窗或结束放大；D-Bus 卸载效果后 `isEffectLoaded` 为 `false` 且原窗恢复；关闭源窗或改变输出布局时正常退出并可重新选窗 |
| 多缩放比例复测 | 在 100% 和 150% 缩放下复测以上核心项，且切焦点与关闭窗口各试 10 轮 |

> 若任意环节出现输入误投、按键卡住或焦点异常，应立即解除放大、记录实际接收窗口并将该组合判为失败。

---

## 4. 实测结果汇总

### 4.1 线性交互（KWin 6.3.6 QPainter，Debian 13）

测试环境：KWin 6.3.6（QPainter 后端）、XWayland 24.1.6、单 1600×1000 @ 60 Hz 虚拟输出。

| 源窗协议与输出缩放 | 持续更新（≥15s） | 九点 / 滚轮 / 按键 | 黑边隔离 | 切焦点退出 |
| --- | --- | --- | --- | --- |
| 原生 Wayland · 100% | 通过 | 1–9 依序命中，滚轮/A 键直达 | 通过 | 10/10 |
| XWayland · 100% | 通过 | 1–9 依序命中，滚轮/A 键直达 | 通过 | 10/10 |
| 原生 Wayland · 150% | 通过 | 1–9 依序命中，滚轮/A 键直达 | 通过 | 10/10 |
| XWayland · 150% | 通过 | 1–9 依序命中，滚轮/A 键直达 | 通过 | 10/10 |

- **其余验证**：弹窗退出、按住主键快捷键取消、再次选窗、关闭源窗、`Esc` 取消、活动时卸载插件、输出缩放变更退出均已通过；并修正了初版失焦 XWayland 过早检查焦点、按住主键退出多发点击及 150% 缩放 XWayland 裁剪问题。
- **统一入口复测**：从桌面启动器点击 **Choose Window…** 与命令行 `picalens toggle` 均通过 100% / 150% 复测（证据目录：`/workspace/scratch/picalens-lab/evidence-kwin-plugin/` 与 `evidence-unified/`）。
- **待验收项**：物理显卡 DRM/OpenGL 路径、Portal 窗口流、两种缩放下 10 轮关闭窗口及 AMD Phoenix 延迟指标。

### 4.2 FSR 1 与 SGSR 交互（KWin 6.7.5 DRM/VirGL，Ubuntu 26.10）

测试环境：KWin 6.7.5（DRM + VirGL OpenGL 4.3）、1280×768 @ 100%，全屏等比适应。后台在确认 `first frame drawn filter=` 成功后才启用坐标换算。

| 滤镜 | 源窗协议 | 九点 / 滚轮 / A 键 | 黑边隔离 |
| --- | --- | --- | --- |
| FSR 1 | 原生 Wayland | 1–9 依序命中，滚轮/A 键直达 | 通过 |
| SGSR | 原生 Wayland | 1–9 依序命中，滚轮/A 键直达 | 通过 |
| FSR 1 | XWayland | 1–9 依序命中，滚轮/A 键直达 | 通过 |
| SGSR | XWayland | 1–9 依序命中，滚轮/A 键直达 | 通过 |

- **像素级对照**：固定九宫格区域截图逐像素对比确认 FSR 1、SGSR 与线性滤镜的输出互不相同；四组均可通过 `toggle` 正常恢复。
- **150% 复测**：已在 1920×1080 @ 150% 下复测 XWayland + SGSR 完整输入。物理显卡性能与其余 150% 组合待验证。
