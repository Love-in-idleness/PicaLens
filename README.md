# PicaLens（鹊镜）

实时放大 Plasma 6 Wayland 会话中已打开的窗口。PicaLens 读作 *PIE-kuh lens*。

## 安装 Debian 预览版

适用于 Debian 13（trixie）amd64 和 Debian 的 KWin `4:6.3.6-1`。下载 `.deb` 后安装，应用、桌面启动器和 KWin 后端会一起部署。 [GitHub Releases](https://github.com/Love-in-idleness/PicaLens/releases)

```sh
sudo apt install ./picalens_0.0.1-1_amd64.deb
```

## 使用

打开 `picalens`，点击 **Choose Window…**，选择一扇已打开的窗口。再次打开 PicaLens 或执行 `picalens toggle` 即可停止。也可以把 `picalens toggle` 设为桌面快捷键，用同一个键开始、取消选窗和停止。选窗时按 Esc 可取消。

在部署了匹配后台组件的 Plasma 6 Wayland 会话中，应用会自动启用 KWin 交互模式。画面等比放大，可直接点击、滚动和使用键盘。原生 Wayland 和 XWayland 源窗使用同一入口。当前后台针对 KWin 6.3.6 构建。 [Plasma 构建与验收](docs/kwin-experiment.md)

缺少兼容的 KWin 后台时，应用通过桌面的 ScreenCast Portal 打开仅显示画面的全屏预览，按 Esc 退出。该模式要求 Portal 提供窗口捕获和正在运行的 PipeWire。

## 开发构建

构建需要 Rust ≥ 1.89、C 编译工具链、libclang、`pkg-config`、GTK ≥ 4.8、libadwaita ≥ 1.4 和 PipeWire 开发库。

```sh
cargo build --locked
target/debug/picalens
```

桌面启动器位于 [`data/`](data/)。本地安装时，让启动器的 `Exec=picalens` 能在会话的 `PATH` 中找到应用，再将 `.desktop` 文件放入桌面应用目录。Plasma 交互模式还需部署匹配的 KWin 后台组件。

在 Debian 13 上可用 `bash packaging/build-deb.sh` 生成安装包与校验文件，输出在 `build/deb/`。构建依赖见[发布流程](.github/workflows/deb-preview.yml)。

### 技术验证

仓库包含原生 Wayland 和 XWayland 测试源窗。开发探针可分别检查 Portal 捕获和实验性 EIS 输入。测试前先打开源窗。

```sh
target/debug/picalens-test-window             # 原生 Wayland 源窗
target/debug/picalens-test-window --xwayland  # XWayland 源窗
target/debug/picalens-probe                # Portal 捕获探针
```

KWin 6.3.6 的 QPainter 测试会话已覆盖两种源窗在 100% 和 150% 缩放下的点击、滚轮和按键。DRM/OpenGL 与实机性能仍待验收。GNOME 50.1 的 Portal 只读捕获已验证。 [验证记录](docs/verification.md)
