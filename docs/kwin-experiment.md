# Plasma Wayland 交互后台

PicaLens 通过 KWin 选择、放大并操作已有窗口。应用负责启动、取消和结束会话，KWin 负责绘制与输入。后台支持 Debian 13 的 KWin 6.3.6 和 Ubuntu 26.10 的 KWin 6.7.5。原生 Wayland 和桌面自带 XWayland 各验收一轮，源应用在整个过程保持运行。 [Portal 验证手册](verification.md)

## 构建环境

后台使用 KWin 的私有 C++ 接口。使用与目标会话的 KWin 二进制来自同一套发行版构建的开发包和运行库。构建需要 C++23、CMake ≥ 3.24、Ninja、Qt ≥ 6.8、ECM、libepoxy 和匹配的 KWin 开发包。使用 KWin 6.7.5 时还需要 libdrm 和 Vulkan 开发包。发布验收使用具备 DRM 与 OpenGL 合成的 Wayland 会话，并启用 XWayland。

Debian 13 试验环境的 `kwin-dev` 和 `kwin-wayland` 包版本均为 `4:6.3.6-1`，搭配 Qt 6.8.2 和 KDE Frameworks 6.13.0。

在仓库目录构建并安装到本地试验目录。

```sh
cmake -S kwin -B build/kwin -G Ninja -DKDE_INSTALL_PLUGINDIR=plugins
cmake --build build/kwin
cmake --install build/kwin --prefix "$PWD/build/kwin-stage"
cargo build --locked
```

## 启动和恢复

启动独立测试会话的 KWin 之前，将暂存目录加入其 Qt 插件搜索路径。

```sh
export QT_PLUGIN_PATH="$PWD/build/kwin-stage/plugins${QT_PLUGIN_PATH:+:$QT_PLUGIN_PATH}"
```

暂存文件应位于 `build/kwin-stage/plugins/kwin/effects/plugins/picalens.so`。在测试桌面运行应用并点击 **Choose Window…**。应用启动选窗时会自动加载后台组件。重新构建后台后，重启测试会话以加载新库。

```sh
target/debug/picalens
# 聚焦目标窗口后，用快捷键或以下命令切换放大
target/debug/picalens toggle
```

从界面选择窗口时，光标变成十字后点击已打开的普通窗口。快捷键直接放大当前活动窗口。在同一输出上检查等比画面，确认 KWin 日志中 `picalens started protocol=` 对应本轮源协议，出现 `picalens first frame drawn` 后开始试验输入。选窗时按 Esc 或再次调用 `picalens toggle` 可取消。放大时用 `picalens toggle` 停止，源窗可继续操作。

这个版本转发已完成的点击、滚轮和正常焦点下的键盘输入。鼠标移出画面、明显拖动或退出时，尚未完成的点击取消。XWayland 应用报告的客户区可能包含透明的客户端阴影。

活动期间断开应用的 D-Bus 连接会结束会话。开发时可用下列命令确认版本支持、已加载状态，以及卸载时的清理行为。

```sh
qdbus6 org.kde.KWin /Effects org.kde.kwin.Effects.isEffectSupported picalens
qdbus6 org.kde.KWin /Effects org.kde.kwin.Effects.isEffectLoaded picalens
qdbus6 org.kde.KWin /Effects org.kde.kwin.Effects.unloadEffect picalens
```

## 测试窗口

在两种模式下分别启动源窗。旧窗口可以保留，用日志和选择器标题判断这轮选中了哪一扇。测试前将误投观察窗布置在目标输出的另一处，整个试验期间保持运行。

```sh
cargo build --locked
target/debug/picalens-test-window
target/debug/picalens-test-window --xwayland
target/debug/picalens-test-window --sentinel
```

实际使用时在仓库目录打开不同终端。记录 `kwin_wayland --version`、`qdbus6 org.kde.KWin /KWin supportInformation` 以及各源窗的 `event=mapped` 和 `event=layout` 行，确认 `fixture=wayland` 或 `fixture=xwayland`。XWayland 轮使用 `xdpyinfo` 确认 XWAYLAND 扩展。

## 验收

在两种源窗上分别使用下表。启动时从合成器选择当前已经打开的源窗，然后观察画面更新和输入日志。记录输出分辨率和缩放，并保留操作前后的截图及源窗、弹窗和观察窗日志。

| 操作 | 核对项 |
| --- | --- |
| 弹出合成器选择器，选中目标窗口 | 放大视图显示所选窗口且比例正确，源窗进程和剧情状态连续 |
| 保持放大至少 15 秒，在画面与黑边间移动指针 | 计数和色块持续变化，光标出现在实际指针位置。源窗隐藏光标时仍能看到桌面主题的箭头 |
| 按从左到右、从上到下的顺序点击九个按钮 | 源窗的 `event=hit target=` 恰好记录 1–9，剧情逐次推进，观察窗没有对应输入 |
| 在画面内滚动并按一个普通键 | 原窗收到对应滚轮和按键按下、释放，观察窗没有对应输入 |
| 点击和滚动黑边 | 源窗和观察窗没有对应输入 |
| 在任意按钮上按住主键，通过快捷键退出，再移到画面外松开 | 源窗和观察窗都没有收到点击，重新选择后正常点击仍可用 |
| 在放大画面内打开测试弹窗，再单独验证切焦点 | 记录放大退出及 `picalens stopped:` 的原因，核对弹窗和观察窗的命中、按下状态是否释放 |
| 聚焦源窗后按 Ctrl+Super+Z，再按一次 | 确认第一次直接放大源窗，第二次恢复普通大小，焦点留在源窗 |
| 选窗时按 Esc 或调用 `picalens toggle`，放大时再次调用 `picalens toggle` | 确认选择器与视图关闭，窗口恢复普通布局，PicaLens 可以再次选窗 |
| 在放大时通过 D-Bus 卸载效果 | `isEffectLoaded` 返回 `false`，放大和输入映射解除，原窗可正常操作 |
| 在放大时关闭源窗或改变输出布局 | 放大视图结束，之后可重新选择。关闭测试包含窗口自行消失的情况 |
| 在 100% 与 150% 缩放下复测 | 记录每种缩放的九点、黑边、弹窗和焦点结果，以及切焦点和关闭窗口各十轮 |

如试验出现误投、卡住的按键或异常焦点，立即解除放大并记录实际接收窗口。该组合的交互验收标为失败。

## 实测结果

隔离会话使用 Debian 13、KWin 6.3.6 的 QPainter 后端、XWayland 24.1.6 和单个 1600×1000 @ 60 Hz 虚拟输出。两扇源窗都在放大操作前启动，观察窗全程运行。计数截图间隔至少 15 秒。九点、滚轮、按键和黑边按采样时段核对源窗、观察窗及另一扇源窗的日志。

| 源窗和输出缩放 | 放大期间持续更新 | 九点、滚轮、按键 | 黑边点击及滚轮 | 切焦点 |
| --- | --- | --- | --- | --- |
| 原生 Wayland · 100% | 通过 | 1–9 依序命中，滚轮和 A 键到达源窗 | 无输入到达任何测试窗 | 10/10 次退出放大 |
| XWayland · 100% | 通过 | 1–9 依序命中，滚轮和 A 键到达源窗 | 无输入到达任何测试窗 | 10/10 次退出放大 |
| 原生 Wayland · 150% | 通过 | 1–9 依序命中，滚轮和 A 键到达源窗 | 无输入到达任何测试窗 | 10/10 次退出放大 |
| XWayland · 150% | 通过 | 1–9 依序命中，滚轮和 A 键到达源窗 | 无输入到达任何测试窗 | 10/10 次退出放大 |

两种源窗都完成了弹窗退出、按住主键时用快捷键取消、再次选择和关闭源窗测试。已测试 Esc 取消选窗、活动时卸载插件和输出缩放改变时退出。初版在选中失焦的 XWayland 窗口时过早检查焦点，按住主键退出时也出现额外点击。修正后等待源窗激活，只投递完整的点击。切换到 150% 后，XWayland 裁剪改为使用完整客户区，并复测了画面与输入。

日志与截图位于云工作区的 `/workspace/scratch/picalens-lab/evidence-kwin-plugin/`。真实显卡的 DRM/OpenGL 路径、Portal 窗口流、两种缩放下的十轮关闭窗口和 AMD Phoenix 延迟指标仍待验收。

### 统一入口复测

在同一 KWin 6.3.6 环境中，从桌面启动器后点击 **Choose Window…**，也从命令 `picalens toggle` 直接选窗。首次选窗时应用自动加载后台。原生 Wayland 与 XWayland 源窗在 100% 缩放下各依序收到 1–9 的点击，以及滚轮和按键。黑边的点击和滚轮未送达源窗。两类源窗在 150% 下又各复测中央点击、滚轮和按键。

Esc 和第二次 `toggle` 均可取消选窗。关闭源窗或强制结束 PicaLens 后，会话结束并释放输入，可再次选窗。GNOME 50.1 实测通过同一命令启动 Portal 只读全屏预览，预览持续出帧，点击未送到源窗，Esc 退出。统一入口的复测记录位于 `/workspace/scratch/picalens-lab/evidence-unified/`。
