# Phase 1/2 架构与关键决定

当前为 `0.2.0`：完成静态实现与离线验证，等待真实设备及原生多指注入验收。Phase 0 的历史实测保持独立，不作为本次多指实现已经通过的证据。

## 模块与线程

| 模块 | 职责 |
|---|---|
| `main.rs` | 命令行参数校验、离线日志校验入口 |
| `hid.rs` | Raw Input 枚举、HidP 能力字段/数据提取、RAWHID 对齐及边界检查 |
| `decode.rs` | 无 Windows API 的字段解释、混合报告组帧与绝对映射 |
| `touch.rs` | 无 Windows API 的事务式触点规划、稳定槽位、完整帧、提交/重试/取消 |
| `injection.rs` | Win32 transport：命中检查、POINTER_TOUCH_INFO 数组、一次 InjectTouchInput 提交 |
| `scenarios.rs` | 合成逻辑场景；单独生成数据不调用原生注入，供离线测试与手动 self-test 共用 |
| `experiment.rs` | 输入工作线程、最小 PT_TOUCH 接收窗口、退出协作、彩色轨迹及两指缩放演示 |
| `verify.rs` | 离线日志生命周期、整帧及坐标校验，不调用触摸 API |
| `logging.rs` | 有界日志队列、独立写线程、丢失计数，拒绝覆盖既有运行日志 |

Raw Input 和注入在同一个工作线程内按顺序执行，避免排队期间丢掉 Down/Up。接收窗口独立处理消息。绘制只复制短时显示快照，释放统计锁后再调用 GDI，避免持锁绘图阻塞输入。日志队列容量 16,384，满队列会计数；日志缺失不能视作验收通过。

## 输入及报告组装

Raw Input 枚举只选择 Digitizers / Touch Pad (`0D:05`)。缓存所选设备的 `RIDI_PREPARSEDDATA`，由 HidP 导出 Report ID、数据索引、LinkCollection、按钮与坐标范围，没有硬编码本机位偏移。原始缓冲区按 `dwSizeHid * dwCount` 遍历，检查对齐、长度及溢出。前台 `WM_INPUT` 交给 DefWindowProc 清理。

每个完整帧使用 Contact Count 指定的记录数，包括 Tip=false 的释放记录。混合帧必须通过同一 Scan Time 组装；不完整新帧、重复 ID、孤立的活动 Count=0 续报、没有 Scan Time 的分片或超过 100 ms 的组帧等待均报错并取消。Count=0 且没有活动 Tip 的空闲记录可以有旧坐标。当前不支持的值数组/复杂 LinkCollection 布局明确报错。

输入超过 500 ms 没有报告时取消活动注入，要求重新观察到空帧；这可能影响停止发送静止报告的设备，必须通过长按实测确认。没有自动读取/修改触控板 Feature Report。

## 事务式触点规划

`touch::Planner` 将原始 Contact ID 映射到注入槽位 `0..capacity-1`，容量为 1..5。它生成整帧及候选成功状态；`Engine` 只有在 transport 成功后才提交该状态。失败不会把未成功注入的位置或新手指写成活动状态。

- 容量 1：保留同一原始 ID；被选手指抬起后先结束，再等所有物理 Tip 抬起，避免另一根手指跳入轨迹。
- 容量 2..5：跟踪所有可信活动触点。原始记录顺序或 ID 数值大小变化不会改变已有槽位。超过容量报错取消，不静默丢手指。
- 每个提交帧包含所有先前活动槽位。某指 UP 时，其他手指仍有 UPDATE。
- 有空闲槽位时，同一输入帧中的 UP、UPDATE、DOWN 合并提交。旧槽位在携带 UP 的帧中仍然占用；当替换会超容量时，先提交释放帧，再复用已成功释放的槽位。该必要拆分不能宣称为单次原子替换。
- 没有额外旋转、平滑或校准；按描述符范围映射至固定物理像素矩形，使用 `(length - 1)`、取整和限幅。

按 Microsoft Touch Injection 约束：Down 为 `INRANGE|INCONTACT|DOWN`，移动为 `INRANGE|INCONTACT|UPDATE`，Up 使用独立 `UP` 结束，取消使用 `CANCELED|UP`。不会使用会留下 hover 的 `INRANGE|UP`。UP 使用最后成功位置；若刚 Down 尚无 UPDATE，会先补 UPDATE。清理先补完整 UPDATE，再提交所有取消 UP。

## 注入、重试及失败状态

`InitializeTouchInjection(capacity, TOUCH_FEEDBACK_NONE)`，指针 ID 使用容量范围内的槽位。未自定义时间戳、压力、方向和接触面积。

`ERROR_NOT_READY (21)` 以 1 ms 间隔重试同一整帧，最多 20 次。成功前不推进状态，也不会跳到下一个输入帧。`ERROR_INVALID_PARAMETER (87)` 按文档视作系统已取消活动注入，清除该状态并等待空帧。其他失败先尝试取消，仍失败时保留已知触点及 `cleanup_pending`，阻止新的 Down；之后只尝试清理并重新等待空帧。

大约 16 ms 检查一次保活；保活也是完整活动触点帧。每个正常 Down/Update 都通过 WindowFromPoint 校验命中专用接收窗口；清理不因遮挡而被该检查拦住，以便结束已有序列。窗口移动、最小化、设备移除、显示变化触发停止/取消，没有后续阶段的重新连接或动态目标管理。

## 接收窗口及退出

UI 只统计 GetPointerInfo 确认为 `PT_TOUCH` 的消息，并调用 GetPointerTouchInfo。各系统 pointer ID 单独保存轨迹，活动触点分配不同颜色并显示 ID；两指距离比来自收到的实际坐标，演示方框按此缩放。它验证接收数据，没有证明所有第三方应用都识别缩放手势。

Esc、WM_CLOSE、Ctrl+C 或计时结束设置停止标志；工作线程先完成清理，再发布完成标志。窗口继续泵消息至少约 300 ms 后关闭，尽量接收最后 UP。汇总记录两端剩余触点、清理失败与日志丢失；残留状态不能被报告为成功。强杀进程、崩溃、锁屏及系统拒绝清理仍可能越过此协作流程。

## 离线日志校验

格式 2 将一次 API 成功记录为 `INJECT_BATCH`，随后记录所有槽位的 `INJECT_OK`。校验器检查每批计数、完整活动集合、槽位唯一性、容量和 UP 坐标。Windows 返回的 ID 不等于注入槽位，也可能交错投递不同手指消息并合并 UPDATE，因此按完整生命周期及有序坐标子序列匹配，并用一对一匹配避免把相同坐标的两个手指重复归到同一序列。

缺少接收事件、Down/Up 不完整、错误坐标、原始 ID 中途改变、日志丢失或未完成关机记录都不能判为通过。验收结果仍需人工确认真实硬件操作，不能由合成场景自动替代。

## 范围及参考

Phase 3–5 的任意目标、完整 GUI、快捷键配置、托盘与发布机制没有加入。未复用 Tapview 未确认许可的源码；MIT 与第三方通知见根目录 LICENSE 和 `third-party-notices.md`。

- [Touchpad collection](https://learn.microsoft.com/en-us/windows-hardware/design/component-guidelines/touchpad-windows-precision-touchpad-collection)
- [InjectTouchInput 状态、完整帧与错误约束](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-injecttouchinput)
- [InitializeTouchInjection](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-initializetouchinjection)
- [GetPointerFrameInfo 及消息合并说明](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getpointerframeinfo)
- [Tapview Windows backend，仅参考](https://github.com/JohnAZoidberg/tapview/blob/main/src/input/windows_backend.rs)
