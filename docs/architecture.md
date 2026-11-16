# Phase 0 架构与边界

## 范围决定

依据 `PadBridge.md` 第 14、17 节：本阶段只验证核心链路。多指原始采集属于可行性证据；同步多指注入、手势、完整 UI、配置文件、目标窗口选择、快捷键体系、托盘与安装包不在本次范围。

源文件职责：

| 文件 | 职责 |
|---|---|
| `main.rs` | 最小命令行参数：probe / capture / self-test |
| `hid.rs` | Raw Input 设备发现、对齐缓冲区、HidP 字段发现和数据提取、RAWHID 边界检查 |
| `decode.rs` | 不依赖 Windows API 的字段解释、混合报告组帧、Contact ID/Tip/Confidence/坐标、绝对映射 |
| `injection.rs` | 单触点注入、稳定 ID 0、成功坐标缓存、重试、保活和释放 |
| `experiment.rs` | 隐藏输入窗口线程、最小测试接收窗口、诊断计数、合成 API 对照实验 |
| `logging.rs` | 独立写日志线程、有界队列、溢出计数 |

## 输入路径

`GetRawInputDeviceList / GetRawInputDeviceInfoW` 筛选 Digitizers / Touch Pad (`0D:05`)。缓存所选设备的 `RIDI_PREPARSEDDATA`，由 `HidP_GetCaps / GetValueCaps / GetButtonCaps` 枚举真实字段，再用 `HidP_GetData` 提取报告中的数据索引。

没有硬编码本机报告的位偏移或坐标范围。Report ID、按钮字段与 LinkCollection 均来自 HID parser。`decode.rs` 接收普通 `Field` 和索引值数组，与 Windows API 分离。当前仅支持同一 LinkCollection 中包含 ID/Tip/X/Y、带 Contact Count 的 PTP 布局；不支持的数组/布局明确报错。

本机 14 字节报告每条只带一个 Contact Record；首条 Contact Count 为 N，后续同一 Scan Time 的 N-1 条为 0。`Assembler` 集齐所有记录后才提交完整帧。Tip=false 的显式抬起记录仍计入 Contact Count。新帧抢先到达、重复 ID、Scan Time 改变或超时均不能被悄悄当成完整数据。

`WM_INPUT` 按 `dwSizeHid * dwCount` 遍历所有报告，使用对齐存储并检查可变尾部范围。所选设备通过原始设备句柄区分；前台 `WM_INPUT` 继续经过 `DefWindowProcW` 清理。

## 注入与接收

Raw Input 和注入在同一工作线程内串行执行，UI 接收窗口在独立线程上。没有跨线程积压触摸帧，也没有逐指鼠标模拟。日志由容量 16,384 的队列交给写线程；溢出会计数，不能据有日志丢失的实验宣称完整证据。最多保留 100,000 个耗时样本和 5,000 个显示点，适用于短时实验。

Per-Monitor DPI Awareness V2 在窗口创建前启用。固定映射矩形由接收窗口创建时的物理客户区坐标计算一次；使用白皮书中的 `(length - 1)` 公式、四舍五入和限幅。窗口移动后停止实验，不实现动态目标映射。

`InitializeTouchInjection(1, TOUCH_FEEDBACK_NONE)`；注入唯一 ID 为 0。原始多指中只选择一个稳定 Contact ID，后续继续追踪同一 ID。被选中手指离开后，先释放，再等所有手指抬起才开始新序列，避免切换手指导致跳跃。Confidence=false 的触点不注入。首次开始也等待一次空触点帧。

DOWN 使用 `INRANGE|INCONTACT|DOWN`，移动/保活使用 `INRANGE|INCONTACT|UPDATE`。UP 使用最后成功注入位置，先补最后一帧 UPDATE，再以单独 `UP` 结束。这里没有照抄白皮书表格中的 `INRANGE|UP`：官方文档说明这种组合会进入 hover，`UP` 才结束触摸序列。未指定压力、方向和接触面积，使用默认值。

`ERROR_NOT_READY` 以 1 ms 间隔重试同一帧，最多 20 次；不会跳过 Down/Up。未自定义时间戳。约 16 ms 定时检查保活；超过 500 ms 没有原始报告时取消活动注入并要求重新观察到空帧。无法证明异常退出/强制杀进程的释放可靠性，见限制。

每个注入位置先 `WindowFromPoint` 检查是否命中本测试窗口。没有向任意第三方窗口发送输入的实现。`WM_POINTERDOWN / UPDATE / UP` 回调中再次调用 `GetPointerInfo`，只统计 `PT_TOUCH`，并调用 `GetPointerTouchInfo` 验证触摸类型。绿色轨迹只使用这些真正收到的屏幕坐标；不会依据原始输入或成功 API 次数伪造接收轨迹。

## 验证证据

合成 `--self-test` 与物理 `--capture` 明确标注来源。`Analyze-Run.ps1` 检查：注入序列合法性、UP 坐标、系统接收 Down/Up 配对、活动触点残留，以及接收事件是否是成功注入事件的有序坐标子序列。Windows 可能合并 UPDATE，因此不要求 UPDATE 消息数量与注入数量完全一致。

`tests/fixtures` 在本机保存真实五指报告和 Windows 预解析数据；录制文件不上传到 Git。两项可选离线回归在运行时读取这些本地文件，默认忽略；通用测试和正常构建不依赖它们。录制回归不代替物理测试，也不是通用 HID 描述符语料库。

## 参考与许可证边界

先阅读了 Tapview 的 Windows 采集及发现实现，以核对 Raw Input / HID API 路线。当前查看的仓库没有可确认的源码复用许可，因此本项目独立实现，没有复制其源码，也没有使用其触控板 Feature Report 写入功能。

- [Tapview Windows backend](https://github.com/JohnAZoidberg/tapview/blob/main/src/input/windows_backend.rs)
- [Tapview Windows discovery](https://github.com/JohnAZoidberg/tapview/blob/main/src/discovery/windows_discovery.rs)
- [Raw Input device metadata](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getrawinputdeviceinfow)
- [Windows Precision Touchpad Collection](https://learn.microsoft.com/en-us/windows-hardware/design/component-guidelines/touchpad-windows-precision-touchpad-collection)
- [InjectTouchInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-injecttouchinput)
- [Microsoft touch injection sample](https://github.com/microsoft/Windows-classic-samples/blob/main/Samples/InjectTouchPointerInput/cpp/InjectTouch.cpp)

PadBridge 原创代码及随附文档采用 MIT 许可证，版权人为 mouse233；白皮书内容保留原样。依赖为 windows-rs 项目的 `windows-sys`、`windows-link`（许可证为 MIT OR Apache-2.0，以各自 crate 元数据/许可证为准）；保留 Cargo.lock，第三方权利及通知见 `docs/third-party-notices.md`。
