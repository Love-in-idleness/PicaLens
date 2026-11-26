# Hyprland 原窗口交互实验

PicaLens 的等比全屏预设可使用 [Hyprland 后台组件](../hyprland/presentation.cpp)。组件在合成器内缩放原窗口，按同一比例换算鼠标坐标。键盘继续交给源窗。PicaLens 通过本地套接字启动选窗和放大，连接关闭时结束会话。

测试使用 Hyprland `0.53.3+ds-4` 及匹配的开发头文件。构建需要 CMake ≥ 3.24、C++23、`pkg-config`、Hyprland 和 GLES 开发头文件。组件加载时会检查头文件与运行中的合成器是否匹配。以下命令从 PicaLens 源码目录在隔离会话运行。

```sh
cmake -S hyprland -B build/hyprland -DCMAKE_BUILD_TYPE=Release
cmake --build build/hyprland
hyprctl plugin load "$(realpath build/hyprland/picalens-hyprland.so)"
cargo build --locked --bin picalens
```

打开 `target/debug/picalens`，选择等比适应和全屏，再选择线性、FSR 1 或 SGSR。点击「选择窗口」后点选原窗口，也可以运行 `target/debug/picalens toggle` 选窗。Esc 或再次运行 `toggle` 可以结束。GPU 滤镜需要 OpenGL ES 3.1 或更新版本、正常方向的输出，以及足够容纳放大画面的输出尺寸。

在 Ubuntu 26.04 的 Hyprland 0.53.3 虚拟机中，使用 DRM、VirGL 和 640×480 输出，缩放为 100%。两种 GPU 滤镜与两类源窗口的四种组合都通过九点点击、滚轮和 A 键测试。留边点击和滚轮未送达源窗或观察窗。对同一区域进行截图逐像素比较，确认两种滤镜的画面彼此不同，且均区别于线性滤镜。物理显卡性能与其他输出比例仍待验证。
