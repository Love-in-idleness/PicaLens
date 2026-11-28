# DDE Wayland（Treeland）交互适配｜进行中

> **当前状态**：DDE Wayland 的交互全屏仍在适配中，相关开发暂缓。实验构建已在虚拟机中完成验证，但发行版自带的 Treeland 仍待验收。上游进展见 [Treeland 功能讨论 #1728](https://github.com/linuxdeepin/treeland/issues/1728)。

---

## 1. 架构与支持范围

- **Treeland 实验后端**：负责窗口选择、画面绘制与输入坐标映射。
- **PicaLens 主程序**：自动发现并连接后端，支持通过 **「选择窗口」** 或 `picalens toggle` 启动放大。
- **支持模式与滤镜**：等比全屏下可选 **线性**、**FSR 1** 或 **SGSR**，同时覆盖原生 Wayland 与会话内 XWayland 应用。
- **回退方案**：当发行版仅提供 ScreenCast Portal 窗口采集而无兼容交互后端时，可使用只读预览预设。

虚拟机实验基于 **Treeland 0.12.1** 与 **dde-wlroots 0.20.2.1**，详细结果见 [docs/verification.md](verification.md#dde-wayland-的-treeland-0121)。

---

## 2. 复现步骤

1. 在已安装兼容 Treeland 实验构建的会话中，打开 PicaLens 并在 **「缩放预设」** 选择 **等比适应**、**交互全屏** 及 **线性 / FSR 1 / SGSR**。
2. 点击 **「选择窗口」**（或运行 `picalens toggle`）后点选已打开的目标应用，即可在放大画面中点击、滚动与操作键盘。
3. 按 `Esc` 或再次执行 `picalens toggle` 退出放大；当原窗口关闭、焦点切走、桌面锁屏或切换当前登录会话时，放大也会自动结束。

---

## 3. GPU 滤镜限制

- FSR 1 与 SGSR 需要 **OpenGL ES ≥ 3.1** 或 **OpenGL ≥ 4.3**。
- 后端按源窗口像素密度捕获，着色阶段单边最多放大 2 倍，且受约 830 万像素（`3840×2160` 等量）中间纹理预算限制。
- 若硬件或环境不满足要求，PicaLens 会提示所选超分滤镜不可用，此时可改用线性预设或通过 DDE ScreenCast Portal 使用只读预览。
