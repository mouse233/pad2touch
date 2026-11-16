# PadBridge — Phase 0 技术可行性验证

本仓库目前只实现白皮书第 14 节的 Phase 0：本机 Precision Touchpad 的原始报告采集、固定屏幕矩形映射、**单指** Windows Touch Injection，以及一个直接接收 `WM_POINTER` 的简单测试窗口。没有完整 GUI，也没有提前实现 Phase 1–5。

**2026-11-16 本机结论：Raw Input 可读取最多五个同时活动触点的 ID/Tip Switch/Confidence/X/Y；实际触控板坐标可转换成真实的 PT_TOUCH Down/Update/Up。** 这是短时本机实验结论，不是所有设备或长期稳定性的保证。原始五指采集与多指同步注入是两项不同能力；本版本只注入一个触点。

详细证据、失败实验、数字和限制见 [Phase 0 测试报告](docs/phase0-results.md)，设计说明见 [architecture.md](docs/architecture.md)。

## 构建

需要 Windows x64、Rust MSVC 工具链及 Windows SDK。实际验证工具链为 Rust/Cargo 1.98.1。绑定使用 windows-rs 项目的 `windows-sys 0.61.2`，没有其他运行时依赖。

```powershell
cargo build --release --locked
cargo test --locked
cargo clippy --all-targets -- -D warnings
```

本机依赖已缓存，也可以加 `--offline`。源码和 `Cargo.lock` 一起交付。

默认测试不需要真实触控板录制数据：8 项通用测试运行，2 项本机录制回归测试默认忽略。真实输入样本未上传到仓库；本机保留样本时可以运行 `cargo test --locked -- --include-ignored` 完成全部 10 项测试。详见 [可选录制测试说明](tests/fixtures/README.md)。

## 运行

在项目目录的普通 PowerShell 中运行，**不用以管理员身份运行**。自动化受限执行环境的错误不代表普通桌面兼容性，见测试报告。

```powershell
# 只枚举 Raw Input 0D:05 集合和 HID 字段，不注入
.\target\release\padbridge-phase0.exe --probe

# 只采集物理触控板数据，不注入
.\target\release\padbridge-phase0.exe --capture --seconds 90 --out artifacts\my-raw-test

# 物理触控板 -> 固定矩形 -> 单指真实触摸事件
.\target\release\padbridge-phase0.exe --capture --inject --seconds 90 --out artifacts\my-touch-test

# 独立 API 对照实验：使用合成坐标，不证明硬件兼容
.\target\release\padbridge-phase0.exe --self-test --seconds 20 --out artifacts\my-api-test

# 检查真实事件的生命周期与坐标是否能匹配成功注入序列
.\scripts\Analyze-Run.ps1 -RunDirectory artifacts\my-touch-test
```

也可以用 `scripts\Run-Phase0.ps1 -Mode capture -Inject -Seconds 90` 自动生成时间戳目录并分析日志。

有多个触控板时先运行 `--probe`，再用 `--device N` 选择本次采集对象。设备编号仅对本次枚举有效。

## 操作方法

1. 测试窗口出现后，先单指接触并全部抬起一次，让程序观察到空触点帧，再开始新一轮触摸。
2. 单指画圈、拖动、长按、快速轻点。绿色轨迹来自测试窗口实际收到的 `PT_TOUCH` 消息。
3. 依次放上两指到五指。`Peak fingers` 和日志应记录真实原始多指数据；本阶段窗口中始终只有一条注入触摸轨迹。额外手指不会触发捏合缩放。
4. 最后全部抬起，观察 Down/Up 是否配对。按窗口内 Esc、关闭窗口或等待时间结束停止；控制台 Ctrl+C 也会请求停止。

测试窗口固定在桌面左上附近，临时置顶以保持注入区域可见。移动、最小化、遮挡、设备拔出或显示配置变化会中止/暂停注入；没有目标窗口选择或动态映射。Raw Input 不独占触控板，系统鼠标与手势行为仍会继续。

## 输出

每次使用独立的 `--out` 目录；重复指定同一目录会覆盖该目录中的本次实验文件。

- `events.log`：单调时间戳、原始报告十六进制、组装触点帧、每次注入结果、Windows 实际收到的 PT_TOUCH 事件。
- `summary.txt`：报告/帧/触点数、API 错误和重试、接收事件计数及软件侧耗时。
- `descriptor-fields.txt`：HID parser 从本机描述数据导出的字段索引、Report ID、逻辑范围。
- `preparsed-data.bin`：Windows 不透明的预解析数据快照，**不是原始 HID 报告描述符**。
- `analysis.json`：运行分析脚本后生成的生命周期及顺序坐标匹配结果。

本机交付目录保留运行截图、已验证的 Release 可执行文件及 ZIP。这些文件及原始输入日志不纳入 Git；克隆仓库后请按上面的命令构建和生成自己的测试输出。输入数据仅保存到用户指定的本地目录，程序没有网络上传、键盘正文记录或遥测。

## 许可证与仓库隐私

PadBridge 原创源码及随附文档采用 [MIT 许可证](LICENSE)，版权声明为 `Copyright (c) 2026 mouse233`。第三方依赖保留各自许可证，见 [第三方声明](docs/third-party-notices.md)。

仓库不包含本地账户目录、凭据、环境配置、运行日志、截图、二进制文件或真实触控板输入样本。硬件测试报告保留汇总结果和非唯一的型号参数；可选本地录制文件由 `.gitignore` 排除。
