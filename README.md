# PadBridge — Phase 1 + Phase 2 MVP

将 Windows Precision Touchpad 的原始接触坐标映射到固定测试矩形，再使用 Windows 原生 Touch Injection 生成触摸事件。使用 Rust / windows-rs (`windows-sys`)，保留命令行和简单 Win32 触摸测试窗口。

**当前版本：0.2.0，Phase 1/2 静态实现完成，等待统一实测。** 本轮没有启动触摸注入、要求用户操作触控板或生成新的运行截图。编译和离线测试通过，不代表新的多指链路已通过硬件验收。

Phase 0 曾在本机验证五指原始采集及单指真实触摸注入，历史证据见 [Phase 0 报告](docs/phase0-results.md)。当前实现与待测项见 [Phase 1/2 静态验证报告](docs/phase12-results.md)，稍后按 [统一实测清单](docs/phase12-manual-test.md) 验收。

## 本阶段功能

- 发现 Raw Input `0D:05` 触控板集合，按 HID 描述数据解析 Contact ID、Tip Switch、Confidence 和 X/Y。
- 根据 Contact Count / Scan Time 组装并行或混合报告；拒绝缺失片段、孤立活动续报和重复 ID。
- `--contacts 1`：单指 MVP，持续跟踪同一手指，避免另一根手指接管时坐标跳变。
- `--contacts 2..5`：同步多指模式，每个 API 帧包含该帧所有已有触点，支持部分抬起、同时变化和稳定 ID。
- 五个可复用的注入槽位 `0..4`；只在 UP 成功后复用。容量不足的触点替换会先释放旧槽位，再提交新触点。
- 仅在提交成功后推进状态；NOT_READY 重试同一完整帧，失败时取消并等待全部手指抬起，清理失败时保留未解决状态。
- 测试窗口分别显示各触点的彩色轨迹、接收 ID 和活动数量；两指距离变化会缩放测试矩形中的演示方框。
- 退出时先释放触点，接收窗口继续处理消息，之后再关闭；提供离线日志校验。

尚未实现 Phase 3 的任意目标窗口/动态区域映射、Phase 4 的完整 GUI/配置/快捷键体系或 Phase 5 的全面恢复与发布机制。程序不独占触控板，原生鼠标和系统手势会继续工作。

## 构建与离线测试

需要 Windows x64、Rust MSVC 工具链及 Windows SDK。本机工具链为 Rust/Cargo 1.98.1。

```powershell
cargo build --release --locked
cargo test --locked
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
```

当前默认测试为 **43 项通过、2 项本机录制测试忽略**。正常克隆不需要真实触控板或私有录制数据。保留本机录制材料时可运行 `cargo test --locked -- --include-ignored`，本机共 **45 项通过**。这些测试不调用 InitializeTouchInjection / InjectTouchInput，不打开测试窗口；录制测试只离线调用 Windows HID parser。

可执行文件暂沿用原 Cargo 包名：`target\release\padbridge-phase0.exe`，功能已更新为 Phase 1/2。

## 稍后统一实测

本机普通 PowerShell 中运行，无需管理员身份。完整步骤及预期现象见 [统一实测清单](docs/phase12-manual-test.md)。

```powershell
# 仅枚举设备
.\scripts\Run-PadBridge.ps1 -Mode probe

# Phase 1：真实触控板单指测试，2 分钟
.\scripts\Run-PadBridge.ps1 -Mode capture -Inject -Contacts 1 -Seconds 120

# Phase 2：真实触控板多指测试，3 分钟
.\scripts\Run-PadBridge.ps1 -Mode capture -Inject -Contacts 5 -Seconds 180

# 可选：独立原生 API 合成对照，包含单指、两指捏合/展开、部分释放和五指
# 会产生真实触摸注入；仅在准备好时手动运行
.\scripts\Run-PadBridge.ps1 -Mode self-test -Contacts 5 -Seconds 40
```

程序启动后先等待一次所有手指抬起的完整帧：接触一下再全部抬起，窗口从 `WAIT ALL FINGERS UP` 进入 `ARMED` 后开始测试。每次测试保存到新的本地时间戳目录，运行结束后自动校验日志。

Esc、关闭窗口、Ctrl+C 或计时结束会请求释放并停止。保持固定测试窗口可见且不移动；遮挡、最小化或显示变化会暂停/停止注入。测试窗口中的轨迹、活动点及缩放演示均来自实际收到的 `PT_TOUCH`，不是根据 API 成功数直接绘制。

## 直接命令行

```powershell
.\target\release\padbridge-phase0.exe --capture --inject --contacts 5 --device 0 --seconds 180
.\target\release\padbridge-phase0.exe --verify-log artifacts\YOUR_RUN\events.log
```

`--capture` 不加 `--inject` 时只记录原始输入。默认容量为 5，支持 `--contacts 1..5`；多设备用 `--probe` 查看后指定 `--device N`。未指定 `--out` 时自动创建唯一目录；手动指定的目录中已有 `events.log` 时拒绝覆盖。

## 日志及验收边界

本地输出包括原始报告、完整触点帧、每次成功注入的 `INJECT_BATCH` 与各槽位状态、收到的 `PT_TOUCH`、`summary.txt` 及 `analysis.json`。格式 2 日志校验按完整触点生命周期匹配系统 ID 与注入槽位，容许不同手指消息交错及 Windows 合并 Update；缺少 Down/Up、坐标不匹配、丢日志或残留触点都会报错。

`analysis.json` 的通过只表示日志一致，不自动宣布硬件验收通过。真实五指同时注入、捏合效果、长按连续性、退出释放和 Windows 参数兼容性仍需实测。故障清理可能被系统拒绝，程序会报告未解决触点并阻止新的 DOWN；不能保证强杀进程/崩溃时执行清理。

## 许可证与隐私

原创代码及随附文档采用 [MIT](LICENSE)，第三方权利及通知见 [third-party-notices.md](docs/third-party-notices.md)。Tapview 仅作技术参考，没有复用其未确认许可的源码。

运行日志、截图、真实录制夹具、二进制文件、打包文件、凭据及本机配置均由 `.gitignore` 排除。程序没有网络上传、普通键盘正文记录、遥测、鼠标模拟触摸、固件/Feature Report 写入或全局鼠标拦截。
