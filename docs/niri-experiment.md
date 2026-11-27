# niri 原窗口交互实验

PicaLens 的等比全屏预设可使用独立 niri fork 中的 26.04 交互后台。合成器绘制选中窗口的表面，并将指针位置反算为原窗口坐标。键盘继续交给原窗口。应用通过 niri 会话的本地 IPC 启动和结束放大，IPC 断开时结束会话。 [已验证的 niri 提交](https://github.com/Love-in-idleness/niri/commit/b419c06e97a46fb0fe8c9eba7ae8080736c91fa8)

按 niri 的构建说明安装依赖。以下命令构建独立的 niri 测试版。 [niri 构建说明](https://niri-wm.github.io/niri/Getting-Started.html#building)

```sh
git clone --branch picalens/v26.04 --single-branch https://github.com/Love-in-idleness/niri.git niri-picalens
git -C niri-picalens switch --detach b419c06e97a46fb0fe8c9eba7ae8080736c91fa8
cargo build --locked --release --manifest-path niri-picalens/Cargo.toml
```

在隔离的 Wayland 会话中启动 `niri-picalens/target/release/niri`。在该会话中打开已安装的 PicaLens，选择等比适应和交互全屏，再选择线性、FSR 1 或 SGSR。点击「选择窗口」后点选原窗口。按 Esc 或执行 `picalens toggle` 结束。选窗时也可用这两种方式取消。GPU 滤镜需要 OpenGL ES 3.1 或更新版本以及足够容纳放大画面的输出。发行版自带的 niri 仍可使用显式选择的 ScreenCast Portal 预览。

从 fork 全新检出并构建后，在 1280×768、100% 缩放的隔离嵌套会话中复测。线性、FSR 1 和 SGSR 对原生 Wayland、XWayland 的六种组合均持续出帧，并通过九点点击、双向滚轮、键盘和黑边隔离检查。Esc、再次切换和关闭源窗口都能结束放大。 [验证记录](verification.md#niri-2604-原窗口交互)

另在 Ubuntu 26.04 虚拟机的原版 niri 26.04 会话中复测。交互超分正确提示后台不可用。使用窗口化预设通过系统 Portal 选中原生 Wayland 窗口后，PicaLens 持续显示更新的 PipeWire 画面。
