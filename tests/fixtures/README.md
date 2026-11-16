# 可选的本机录制回归测试

本目录中的真实触控板报告与 Windows HID 预解析数据属于本地实验材料，已通过 `.gitignore` 排除，**不随 Git 仓库上传**。仓库只保留测试逻辑和本说明。

正常克隆、构建和 `cargo test --locked` 均不需要这些文件：通用测试正常运行，2 项录制测试默认忽略，当前数量见根目录 README。没有在编译时嵌入本地样本。

保留原实验样本的本机可以使用：

```powershell
cargo test --locked -- --include-ignored
```

此时需具备以下本地文件：

- `asue1a03-preparsed.bin`：2026-11-16 Phase 0 实测的 `RIDI_PREPARSEDDATA` 快照，包含对齐填充。它是 Windows HID parser 的不透明预解析数据，**不是原始 HID Report Descriptor**，跨 Windows 版本可移植性不作保证。
- `five-contacts.hex`：同次实验中五个同时活动触点的一组连续报告；一行一条十六进制报告。第一条 Contact Count 为 5，其余为 0，Scan Time 相同。

可选测试通过 `HidP_GetCaps / GetValueCaps / GetButtonCaps / GetData` 重新解析五指帧，检查触点数量、ID、Tip Switch、Confidence、坐标范围及短报告拒绝。它不能代替真实硬件测试。其他设备若需建立自己的录制夹具，应另行核对其报告布局和期望值。
