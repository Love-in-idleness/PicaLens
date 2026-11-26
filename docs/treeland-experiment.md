# DDE Wayland 的 Treeland 后端

PicaLens 从自己的「选择窗口」或 `picalens toggle` 开始放大。Treeland 0.12.1 的后台负责选窗、绘制和输入，等比全屏可使用线性、FSR 1 或 SGSR。XWayland 指 Treeland 会话内的 X11 应用。

## 后端

交互后端属于 Treeland，EGL 上下文改动属于 dde-wlroots。两个改动已整理为各自的提交，兼容的 Treeland 构建和合成器分支仍待发布。PicaLens 保留会话发现和连接逻辑。发行版提供窗口采集时，可以直接使用 ScreenCast Portal 的窗口预览。 [Treeland](https://github.com/linuxdeepin/treeland) · [dde-wlroots](https://github.com/linuxdeepin/dde-wlroots)

虚拟机验证基于 Treeland 0.12.1 和 dde-wlroots 0.20.2.1。 [验证记录](verification.md#dde-wayland-的-treeland-0121)

## 使用

1. 安装兼容的 Treeland 构建后，在 PicaLens 的「缩放预设」选择等比适应、交互全屏，以及线性、FSR 1 或 SGSR。
2. 点击「选择窗口」，然后点选已打开的应用。放大后可在画面内点击、滚动和使用键盘。
3. 按 Esc 或再次执行 `picalens toggle` 可恢复。原窗口关闭、焦点切走、桌面锁屏或当前登录会话切换也会结束放大。

FSR 1 和 SGSR 需要 OpenGL ES 3.1 或 OpenGL 4.3。后端按源窗口像素密度捕获，着色阶段每边最多放大两倍，并受约 830 万像素的中间纹理预算限制。硬件不满足要求时，PicaLens 会提示所选超分滤镜不可用。可另选线性预设，或通过 DDE 的 ScreenCast Portal 使用窗口预览。
