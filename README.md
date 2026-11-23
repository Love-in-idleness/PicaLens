# PicaLens（鹊镜）

实时放大 Plasma 6 Wayland 会话中已打开的窗口。PicaLens 读作 *PIE-kuh lens*。

## 安装预览版

从发布页选择 Ubuntu 26.10 或 Debian 13 的 amd64 安装包。以 Ubuntu 为例。 [GitHub Releases](https://github.com/Love-in-idleness/PicaLens/releases)

```sh
sudo apt install ./picalens_0.0.2-1ubuntu26.10_amd64.deb
```

## 使用

打开 `picalens`，点击 **Choose Window…**，选择一扇已打开的窗口。再次打开 PicaLens 或执行 `picalens toggle` 即可停止。也可以把 `picalens toggle` 设为桌面快捷键，用同一个键开始、取消选窗和停止。选窗时按 Esc 可取消。

在 Plasma 6 Wayland 会话中，系统 KWin 与包内后台匹配时，应用会自动启用交互模式。画面等比放大，可直接点击、滚动和使用键盘。原生 Wayland 和 XWayland 源窗使用同一入口。PicaLens 的安装及 KWin 的更新可独立进行。 [Plasma 构建与验收](docs/kwin-experiment.md)

在 GNOME Wayland 上，从应用菜单打开 PicaLens，点击 **Choose Window…**，再在 GNOME 的系统对话框中选中窗口。PicaLens 通过 ScreenCast Portal 和 PipeWire 提供只读全屏预览，按 Esc 退出。缺少兼容后台的 Plasma 会话也使用这条路径。已用 v0.0.2 发布包在 GNOME 50.1 验证安装、选窗和预览。

## 开发构建

构建需要 Rust ≥ 1.89、C 编译工具链、libclang、`pkg-config`、GTK ≥ 4.8、libadwaita ≥ 1.4 和 PipeWire 开发库。

```sh
cargo build --locked
target/debug/picalens
```

桌面启动器位于 [`data/`](data/)。本地安装时，让启动器的 `Exec=picalens` 能在会话的 `PATH` 中找到应用，再将 `.desktop` 文件放入桌面应用目录。Plasma 交互模式还需部署匹配的 KWin 后台组件。

在 Debian 13 或 Ubuntu 26.10 上可用 `bash packaging/build-deb.sh` 生成安装包与校验文件，输出在 `build/deb/`。构建依赖见[发布流程](.github/workflows/deb-preview.yml)。

### 技术验证

仓库包含原生 Wayland 和 XWayland 测试源窗。开发探针可分别检查 Portal 捕获和实验性 EIS 输入。测试前先打开源窗。

```sh
target/debug/picalens-test-window             # 原生 Wayland 源窗
target/debug/picalens-test-window --xwayland  # XWayland 源窗
target/debug/picalens-probe                # Portal 捕获探针
```

KWin 6.3.6 的 QPainter 测试会话已覆盖两种源窗在 100% 和 150% 缩放下的点击、滚轮和按键。DRM/OpenGL 与实机性能仍待验收。GNOME 50.1 的 Portal 只读捕获已验证。 [验证记录](docs/verification.md)
