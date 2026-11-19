# PicaLens（鹊镜）

实时放大 Plasma 6 Wayland 会话中已打开的窗口。PicaLens 读作 *PIE-kuh lens*。

## 0.0.1 技术验证

当前以 Plasma 6 Wayland 为目标桌面，原生 Wayland 和 XWayland 应用分别验收。独立 X11 会话不在支持范围内。技术验证检查窗口被遮挡后是否继续出帧，以及放大画面的输入能否准确送达源窗口。 [验收范围与结果](docs/verification.md)

KWin 6.3.6 实验插件在合成器内选择、放大并操作现有窗口。原生 Wayland 与 XWayland 已在隔离 Plasma 会话中完成 100% 和 150% 缩放的输入验证。插件需在版本匹配的测试会话中显式加载。DRM/OpenGL 后端和性能仍待实机验收。 [KWin 插件构建与验收](docs/kwin-experiment.md)

此前 GNOME 50.1 已验证只读捕获和全屏覆盖下持续出帧，组合会话的全屏输入曾误投，相关记录保留供比较。

### Portal 探针

探针用 GTK Picture 和 GdkMemoryTexture 等比显示 PipeWire 共享内存帧，捕获时隐藏鼠标指针。GLArea 与 libplacebo 缩放、滤镜、鼠标元数据、Portal 快捷键绑定、DMA-BUF 和安装包在首轮实验之后推进。

当前锁定依赖需要 Rust ≥ 1.89。构建还需要 C 编译工具链、libclang、`pkg-config`、GTK ≥ 4.8、libadwaita ≥ 1.4 和 PipeWire 开发库。运行捕获实验需要当前桌面对应的 XDG Desktop Portal 后端和正在运行的 PipeWire。遮挡实验还需要带 PipeWire 捕获源的 OBS。

在项目目录执行 `cargo build --locked`。每次选择一种源窗模式启动，再从另一终端启动探针。

```sh
target/debug/picalens-test-window             # 原生 Wayland 源窗
target/debug/picalens-test-window --xwayland  # XWayland 源窗
target/debug/picalens-probe
```

在系统选择器中选中已打开的 PicaLens 测试窗。并排、全屏、误投检查和结果记录见[技术验证步骤](docs/verification.md)。
