# PicaLens（鹊镜）

<img src="data/io.github.LoveInIdleness.PicaLens.png" alt="PicaLens 图标，方形黄铜望远镜中的像素鹦鹉" width="144">

实时放大 Wayland 会话中已打开的窗口，也能通过 Gamescope 启动游戏。PicaLens 读作 *PIE-kuh lens*。

## 安装预览版

从发布页选择 Ubuntu 26.10 或 Debian 13 的 amd64 安装包。以 Ubuntu 为例。 [GitHub Releases](https://github.com/Love-in-idleness/PicaLens/releases)

```sh
sudo apt install ./picalens_0.1.0-1ubuntu26.10_amd64.deb
```

升级后重新登录 Plasma，会话会加载新版 KWin 后台。

## 使用

打开 `picalens`，在「放大」页选择预设，然后点击「选择窗口」。可以在「缩放预设」页编辑或复制设置。默认等比适应，另有裁剪铺满、拉伸和整数倍缩放。窗口化、整数倍和最近邻使用只读预览。只读预览中按 F11 切换窗口和全屏，按 F12 保存截图，按 Esc 结束。

Lanczos 3 按预览的物理像素尺寸重采样，适合文字和普通画面。Scale2x 先重建像素画的斜边，再用最近邻缩放到预览尺寸。两个滤镜在后台线程处理最新帧。超过中间图像上限的 Scale2x 使用最近邻。

**FSR 1** 采用 EASU 放大和 RCAS 锐化两个阶段，**SGSR** 采用单阶段的空间放大。两者复用 Gamescope 的 SDR 着色算法。着色器最多放大 2 倍每边，中间图像上限为 3840×2160 等量像素。选择交互全屏时，软件会检查后端和放大尺寸是否支持所选滤镜，无法运行时会给出提示。 [Gamescope 着色器](https://github.com/ValveSoftware/gamescope/tree/7d957c518081c27e95b0f2d3501237abc62bf4f5/src/shaders)

Plasma 6 会话中，兼容的 KWin 后台支持交互全屏，可以点击、滚动和操作键盘。使用 OpenGL 合成时，全屏预设可以同时启用 FSR 1 或 SGSR，原生 Wayland 和 XWayland 窗口均可操作。GNOME 和 Plasma 的「设置」页可修改放大快捷键。在 Plasma 使用交互全屏预设时，快捷键或 `picalens toggle` 直接放大当前活动窗口，再按一次恢复。要手动指定窗口或使用只读预览，请从界面点击「选择窗口」。 [Plasma 构建与验收](docs/kwin-experiment.md)

### niri 和 Hyprland

niri 26.04 和 Hyprland 0.53.3 的 PicaLens 后端支持等比全屏，滤镜可选线性、FSR 1 和 SGSR。两种 GPU 滤镜都在隔离会话中通过了原生 Wayland 和 XWayland 的原窗口输入测试。 [niri 后端](docs/niri-experiment.md) · [Hyprland 后端](docs/hyprland-experiment.md)

只读预览使用 ScreenCast Portal 选窗和 PipeWire 单窗流。niri 会话使用 `xdg-desktop-portal-gnome`，Hyprland 会话使用 `xdg-desktop-portal-hyprland`。Hyprland 旧版选窗器若显示多个来源标签，请切换到 **Window** 再选择应用。 [验收记录](docs/verification.md#niri-与-hyprland-单窗放大) · [niri 采集文档](https://niri-wm.github.io/niri/Screencasting.html) · [Hyprland Portal 文档](https://wiki.hypr.land/hypr-ecosystem/user/xdg-desktop-portal-hyprland/)

`picalens toggle` 打开选窗器，再按一次结束放大。niri 的 `binds` 中可添加 `Mod+F8 { spawn "picalens" "toggle"; }`。使用 Hyprlang 配置的 Hyprland 可添加 `bind = SUPER, F8, exec, picalens toggle`，使用 Lua 配置的版本可将同一命令绑定到执行调度器。

### DDE Wayland（Treeland）

DDE Wayland 使用同一个「选择窗口」入口和 `picalens toggle` 命令。等比交互全屏需要带 PicaLens 后端的 Treeland 构建，可使用线性、FSR 1 和 SGSR 操作原生 Wayland 与 XWayland 应用。兼容的 Treeland 构建仍待发布。窗口预览可使用提供窗口来源的 `xdg-desktop-portal-dde` 与 PipeWire。 [Treeland 后端与使用](docs/treeland-experiment.md)

### GNOME

GNOME 51 Wayland 可启用随包安装的 PicaLens 扩展。首次安装后重新登录 GNOME，再启用扩展。

```sh
gnome-extensions enable picalens@love-in-idleness.github.io
```

使用全屏预设并从 Shell 选窗后，鼠标可直接点击和滚动。Esc 或切换快捷键恢复原窗。扩展直接支持原生 Wayland 窗口，包括使用 Wine Wayland 驱动的游戏。XWayland 交互需要下文的可选组件。窗口化、整数倍和最近邻使用 Portal 只读预览。

#### GNOME 的 XWayland 交互

Ubuntu 27.04 的 Mutter `51.0-1ubuntu3` 已验证。可选组件随 GNOME Shell 启动，为放大中的 XWayland 窗口换算鼠标坐标。它只配置当前账户。系统升级后，如果 Mutter 的软件包版本不同，Shell 会自动使用系统库。未加载组件时，可用「只读全屏预览」预设查看 XWayland 窗口。

在运行 GNOME 的账户中，解压匹配的 `picalens-gnome-xwayland_*.tar.gz`，运行其中的 `./install-user.sh` 后注销并重新登录。安装脚本会打印写入的 systemd 配置路径。移除该文件并执行 `systemctl --user daemon-reload` 后，下次登录恢复使用系统库。

如需为当前版本构建组件，在启用了源码仓库的系统安装 Mutter 构建依赖和 `patchelf`，下载与 `libmutter-51-0` 对应的 Mutter 源码包，然后运行 `bash packaging/build-gnome-xwayland.sh /path/to/mutter_51.x.dsc`。输出位于 `build/deb/`。

「启动游戏」页使用系统安装的 Gamescope。可以保存 Linux 或 Wine 启动配置，也可以复制含 `%command%` 的 Steam 启动选项。分辨率、滤镜、限帧等选项根据本机的 Gamescope 版本提供。游戏经 Gamescope 启动后可独立运行。 [Gamescope 选项](https://github.com/ValveSoftware/gamescope#options)

在「设置 → 外观」可选择跟随系统、浅色或深色，切换立即生效并保存。界面提供简体中文、英文和日文。默认跟随系统，也可以在「设置」页选定语言并重启 PicaLens。

## 开发构建

构建需要 Rust ≥ 1.89、C 编译工具链、libclang、`pkg-config`、GTK ≥ 4.14、libadwaita ≥ 1.4 和 PipeWire 开发库。

```sh
cargo build --locked
target/debug/picalens
```

桌面启动器位于 [`data/`](data/)。本地安装时，让启动器的 `Exec=picalens` 能在会话的 `PATH` 中找到应用，再将 `.desktop` 文件放入桌面应用目录。Plasma 交互模式还需部署匹配的 KWin 后台组件。

在 Debian 13、Ubuntu 26.10 或 Ubuntu 27.04 上可用 `bash packaging/build-deb.sh` 生成安装包与校验文件，输出在 `build/deb/`。构建依赖见[发布流程](.github/workflows/deb-preview.yml)。

### 技术验证

仓库包含原生 Wayland 和 XWayland 测试源窗。开发探针可分别检查 Portal 捕获和实验性 EIS 输入。测试前先打开源窗。

```sh
target/debug/picalens-test-window             # 原生 Wayland 源窗
target/debug/picalens-test-window --xwayland  # XWayland 源窗
target/debug/picalens-probe                # Portal 捕获探针
```

KWin 6.3.6 的 QPainter 测试会话已覆盖两种源窗在 100% 和 150% 缩放下的线性交互。KWin 6.7.5 的 DRM/OpenGL 虚拟机已覆盖两种源窗的 FSR 1 与 SGSR 交互。GNOME 50.1 的 Portal 只读捕获已验证。GPU 滤镜的物理显卡性能仍待验证。 [验证记录](docs/verification.md)
