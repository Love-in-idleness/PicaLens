# Hyprland 原窗口交互后台

PicaLens 的等比全屏预设可加载仓内提供的 Hyprland 后台插件（[hyprland/presentation.cpp](../hyprland/presentation.cpp)）：
- **Hyprland 插件**：在合成器内直接缩放原窗口，按相同比例逆向换算鼠标坐标，键盘输入继续交给源窗口。
- **PicaLens 主程序**：通过本地套接字触发选窗与放大；当连接关闭时，会话自动结束并恢复原窗。

---

## 1. 构建与加载插件

### 依赖要求
- 已验证版本：Hyprland `0.53.3+ds-4` 及匹配的开发头文件（插件加载时会校验头文件版本与运行中合成器是否一致）。
- 构建工具与库：CMake ≥ 3.24、C++23、`pkg-config`、Hyprland 与 OpenGL ES 开发头文件。

### 编译与加载（在 PicaLens 仓库根目录执行）

```sh
cmake -S hyprland -B build/hyprland -DCMAKE_BUILD_TYPE=Release
cmake --build build/hyprland
hyprctl plugin load "$(realpath build/hyprland/picalens-hyprland.so)"
cargo build --locked --bin picalens
```

---

## 2. 启动与使用

1. 运行 `target/debug/picalens`，在预设中选择 **等比适应**、**交互全屏**，以及 **线性 / FSR 1 / SGSR**。
2. 点击 **「选择窗口」** 或运行 `target/debug/picalens toggle`，随后点选目标窗口。
3. 按 `Esc` 或再次执行 `toggle` 结束放大。

**运行要求**：
- GPU 滤镜需要 OpenGL ES ≥ 3.1、正常方向（未旋转）的输出，以及足够容纳放大画面的输出尺寸。

---

## 3. 验证记录

- **测试环境**：Ubuntu 26.04 虚拟机、Hyprland `0.53.3`、DRM/VirGL、640×480 @ 100%。
- **验证结果**：FSR 1 / SGSR 与原生 Wayland / XWayland 全部 4 种组合均通过九点点击、滚轮与 A 键输入验收；留边点击与滚轮未送达源窗或观察窗；固定区域逐像素对比确认 FSR 1、SGSR 与线性滤镜输出互不相同。
- **待验证项**：物理显卡性能与其他输出缩放比例。
