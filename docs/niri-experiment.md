# niri 原窗口交互实验

PicaLens 的等比全屏预设可使用带 [PicaLens 补丁](../niri/v26.04-presentation.patch) 的 niri 26.04。合成器绘制选中窗口的表面，并将指针位置反算为原窗口坐标。键盘继续交给原窗口。应用通过 niri 会话的本地 IPC 启动和结束放大，IPC 断开时结束会话。

按 niri 的构建说明安装依赖。以下命令从 PicaLens 源码目录运行，在单独的目录构建测试版。 [niri 构建说明](https://niri-wm.github.io/niri/Getting-Started.html#building)

```sh
patch="$(pwd)/niri/v26.04-presentation.patch"
git clone --branch v26.04 --depth 1 https://github.com/niri-wm/niri.git niri-test
git -C niri-test apply "$patch"
mkdir -p niri-test/src/render_helpers/picalens-shaders
cp src/shaders/{fsr1,sgsr1,easu-pass,rcas-pass,sgsr-pass}.glsl niri-test/src/render_helpers/picalens-shaders/
cargo build --locked --release --manifest-path niri-test/Cargo.toml
cargo build --locked --bin picalens
```

在隔离的 Wayland 会话中启动 `niri-test/target/release/niri`。该会话中运行 `target/debug/picalens`，选择等比适应和全屏，再选择线性、FSR 1 或 SGSR。点击「选择窗口」后点选原窗口。按 Esc 或执行 `target/debug/picalens toggle` 结束。选窗时也可用这两种方式取消。GPU 滤镜需要 OpenGL ES 3.1 或更新版本以及足够容纳放大画面的输出。发行版自带的 niri 仍可使用显式选择的 ScreenCast Portal 预览。

在 niri 26.04 的隔离嵌套会话中，1280×768、100% 缩放下，两种滤镜与两类源窗口的四种组合均通过九点点击、滚轮和 A 键测试。留边点击和滚轮未送达源窗或观察窗。同一区域的截图逐像素比较确认两种滤镜有独立的渲染结果。物理显卡性能与其他输出比例仍待验证。
