# DDE Wayland 的 Treeland 后端

PicaLens 从自己的「选择窗口」或 `picalens toggle` 开始放大。Treeland 0.12.1 的后台负责选窗、绘制和输入，等比全屏可使用线性、FSR 1 或 SGSR。XWayland 指 Treeland 会话内的 X11 应用。

## 构建

在构建环境中备齐 Treeland 0.12.1 的发行版构建依赖，包括 DDM 0.5.0 或更高版本、Qt 6 Shader Tools、Treeland Protocols 和 wlroots 协议。使用同一 Qt 6 版本构建和运行；Waylib 使用 Qt 私有 API。

```sh
picalens_src="$(pwd)"  # 在 PicaLens checkout 中运行
cd "$(mktemp -d)"
git clone --branch 0.12.1 --depth 1 https://github.com/linuxdeepin/treeland.git
git clone --branch 0.20.2.1 --depth 1 https://github.com/linuxdeepin/dde-wlroots.git
git -C dde-wlroots apply "$picalens_src/treeland/dde-wlroots-0.20.2.1-egl.patch"
git -C treeland apply "$picalens_src/treeland/v0.12.1-presentation.patch"
cp -a "$picalens_src/treeland/presentation" treeland/src/picalens
cmake -S treeland -B treeland/build -G Ninja \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX=/usr \
  -DWITH_SUBMODULE_WAYLIB=ON \
  -DTREELAND_FETCH_DDE_WLROOTS=ON \
  -DFETCHCONTENT_SOURCE_DIR_DDE_WLROOTS="$PWD/dde-wlroots" \
  -DPICALENS_SHADER_DIR="$picalens_src/src/shaders"
cmake --build treeland/build
```

按发行版的 Treeland 打包流程安装上述构建产物，随后重新进入 DDE Wayland 会话。构建将着色器打入 Treeland 资源，PicaLens 使用当前登录会话的后台，无需另设启动命令。 [Treeland 依赖清单](https://github.com/linuxdeepin/treeland/blob/0.12.1/debian/control)

## 使用

1. 在 PicaLens 的「缩放预设」选择等比适应、交互全屏，以及线性、FSR 1 或 SGSR。
2. 点击「选择窗口」，然后点选已打开的应用。放大后可在画面内点击、滚动和使用键盘。
3. 按 Esc 或再次执行 `picalens toggle` 可恢复。原窗口关闭、焦点切走、桌面锁屏或当前登录会话切换也会结束放大。

FSR 1 和 SGSR 需要 OpenGL ES 3.1 或 OpenGL 4.3。后端按源窗口像素密度捕获，着色阶段每边最多放大两倍，并受约 830 万像素的中间纹理预算限制。硬件不满足要求时，PicaLens 会提示所选超分滤镜不可用。可另选线性预设，或通过 DDE 的 ScreenCast Portal 使用窗口预览。
