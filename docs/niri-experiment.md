# niri 原窗口交互后台

PicaLens 的等比全屏预设可配合独立 niri fork 中的 26.04 交互后台运行：
- **niri 交互后台**：负责绘制所选窗口表面并将指针位置反算为原窗口坐标，键盘焦点继续由原窗口持有。
- **PicaLens 主程序**：通过 niri 会话的本地 IPC 启动与结束放大；当 IPC 断开时，后台自动结束放大并恢复原窗。

> 已验证的 niri fork 提交：[`b419c06e97a46fb0fe8c9eba7ae8080736c91fa8`](https://github.com/Love-in-idleness/niri/commit/b419c06e97a46fb0fe8c9eba7ae8080736c91fa8)

---

## 1. 构建独立 niri 测试版

请先参照 [niri 官方构建说明](https://niri-wm.github.io/niri/Getting-Started.html#building) 安装依赖，然后检出对应分支与已验证提交并编译：

```sh
git clone --branch picalens/v26.04 --single-branch https://github.com/Love-in-idleness/niri.git niri-picalens
git -C niri-picalens switch --detach b419c06e97a46fb0fe8c9eba7ae8080736c91fa8
cargo build --locked --release --manifest-path niri-picalens/Cargo.toml
```

---

## 2. 启动与使用

1. 在隔离的 Wayland 会话中启动 `niri-picalens/target/release/niri`。
2. 打开已安装的 PicaLens，在预设中选择 **等比适应**、**交互全屏**，以及 **线性 / FSR 1 / SGSR**。
3. 点击 **「选择窗口」**（或调用 `picalens toggle`）并点选目标窗口。
4. 按 `Esc` 或再次执行 `picalens toggle` 退出放大（选窗期间同样可用这两种方式取消）。

**运行要求与回退说明**：
- GPU 滤镜需要 OpenGL ES ≥ 3.1 及足够容纳放大画面的输出尺寸。
- 发行版自带的原版 niri 暂不支持交互放大，可显式选择 ScreenCast Portal 只读预览预设。

---

## 3. 验证记录

- **fork 交互复测**：全新检出构建后，在 1280×768 @ 100% 隔离嵌套会话中，线性 / FSR 1 / SGSR 与原生 Wayland / XWayland 的全部 6 种组合均持续出帧，并通过九点点击、双向滚轮、键盘与黑边隔离检查；`Esc`、再次 `toggle` 与关闭源窗均可正常结束放大。详见 [docs/verification.md](verification.md#niri-2604-原窗口交互)。
- **原版 niri 26.04 复测**：在 Ubuntu 26.04 虚拟机中确认交互超分会正确提示后台不可用；改用窗口化只读预设后，通过系统 Portal 选中原生 Wayland 窗口可持续显示更新的 PipeWire 画面。
