# Pad2Touch
## Windows Precision Touchpad to Touchscreen
### 技术白皮书与开发实施规范

**Version:** 1.0  
**Date:** 2026-11-16  
**Project:** Pad2Touch  
**Platform:** Windows 10 / Windows 11 x64  
**Primary Language:** Rust  
**Status:** Technical Specification / MVP Development

---

# 1. 项目概述

## 1.1 项目背景

macOS 在 Apple Silicon 设备运行 iOS/iPadOS 应用时，提供了一项名为 Touch Alternatives 的功能。

其中的 Trackpad Capture 可以将 MacBook 触控板转换为虚拟触摸屏。用户可以直接在触控板上移动手指、轻点和执行多指操作，系统将这些动作映射到应用窗口中的对应位置。

Windows 虽然具有 Precision Touchpad（PTP）原始触摸数据读取能力，以及原生 Touch Injection API，但缺少一套将两者结合起来的通用用户态工具。

Pad2Touch 的目标是填补这一空缺。

## 1.2 项目目标

Pad2Touch 是一个 Windows 用户态工具，用于将 Windows Precision Touchpad 转换为虚拟触摸输入设备。

它通过读取触控板的原始 HID 多点触摸报告，获取手指的真实接触状态和坐标，再将其转换为 Windows 原生触摸事件。

核心能力：

- 读取 Precision Touchpad 原始多指触摸数据。
- 将触控板坐标绝对映射到指定窗口或屏幕矩形。
- 使用 Windows 原生 API 注入真实触摸事件。
- 支持单指、多指及连续触摸轨迹。
- 支持多种捕获激活方式。
- 支持窗口移动、缩放后的动态坐标更新。
- 提供可视化配置界面。

## 1.3 设计原则

**必须遵循：**

1. 完全运行在用户态。
2. 不需要安装内核驱动。
3. 不向目标进程注入 DLL。
4. 不修改目标应用 EXE。
5. 不使用针对特定模拟器的专用接口。
6. 不通过鼠标点击模拟触摸。
7. 使用 Windows 原生 Touch Injection API。
8. 不修改触控板固件或 PTP Feature Reports。
9. 默认不需要管理员权限。
10. 不收集或上传用户输入数据。

Pad2Touch 的输入结果必须是真正的 Windows Touch Input，而不是鼠标事件。

---

# 2. 技术选型

## 2.1 核心技术栈

| 模块 | 技术 |
|---|---|
| 编程语言 | Rust |
| Windows API | Win32 API |
| HID 输入 | Windows Raw Input |
| HID 解析 | HID Parser / PTP Report Parser |
| 触摸注入 | Windows Touch Injection |
| UI | egui / eframe |
| Windows API Binding | windows-rs |
| 配置文件 | TOML |
| 构建工具 | Cargo |

优先使用 Rust 实现，尽可能沿用 Tapview 所采用的技术路线。

## 2.2 参考项目：Tapview

GitHub：

https://github.com/JohnAZoidberg/tapview

Tapview 是一个开源触控板可视化工具，能够显示 Precision Touchpad 的多指接触位置、轨迹和原始报告信息。

其 Windows 实现具有重要参考价值：

- 使用 Raw Input 获取 Precision Touchpad 报告。
- 通过 HID Parser 解析触摸数据。
- 使用 SetupAPI/HID API 识别设备。
- 不需要额外驱动和特殊权限。
- 使用 Rust 和 egui 构建界面。

建议 Agent 首先研究以下模块：

- `src/input/windows_backend.rs`
- `src/discovery/windows_discovery.rs`
- `src/ptp.rs`
- `src/multitouch.rs`
- `src/hid/`

**重要：源码复用必须先确认许可证。**

当前参考仓库的可见文件列表没有 LICENSE 文件。未确认授权前，只能将其作为技术和架构参考，不得直接复制、修改或分发其源码。应优先根据公开 HID 规范和 Microsoft API 文档独立实现。

---

# 3. 总体系统架构

Pad2Touch 分为六个主要模块。

```text
┌──────────────────────────────────────┐
│             Precision Touchpad       │
│           HID Multi-touch Reports    │
└──────────────────┬───────────────────┘
                   │
                   ▼
┌──────────────────────────────────────┐
│           Raw Input Backend          │
│                                      │
│ RegisterRawInputDevices              │
│ WM_INPUT                             │
│ GetRawInputData                      │
└──────────────────┬───────────────────┘
                   │
                   ▼
┌──────────────────────────────────────┐
│           PTP Report Parser          │
│                                      │
│ Contact ID                           │
│ Tip Switch / Confidence              │
│ X / Y Coordinates                    │
│ Contact Count                        │
└──────────────────┬───────────────────┘
                   │
                   ▼
┌──────────────────────────────────────┐
│          Contact Manager             │
│                                      │
│ Contact Lifecycle                    │
│ Multi-touch Tracking                 │
│ Down / Move / Up                     │
└──────────────────┬───────────────────┘
                   │
                   ▼
┌──────────────────────────────────────┐
│       Absolute Coordinate Mapper     │
│                                      │
│ Touchpad Coordinates                 │
│          ↓                           │
│ Normalized Coordinates               │
│          ↓                           │
│ Target Rectangle                     │
│          ↓                           │
│ Desktop Screen Coordinates           │
└──────────────────┬───────────────────┘
                   │
                   ▼
┌──────────────────────────────────────┐
│        Windows Touch Injection       │
│                                      │
│ InitializeTouchInjection             │
│ InjectTouchInput                     │
│ POINTER_TOUCH_INFO                   │
└──────────────────┬───────────────────┘
                   │
                   ▼
┌──────────────────────────────────────┐
│          Target Application          │
│                                      │
│ Native Windows Touch Input           │
└──────────────────────────────────────┘
```

UI、设备发现、快捷键和配置管理作为独立模块运行。

所有输入采集和触摸注入工作均不得阻塞 UI 线程。

---

# 4. 输入采集模块

## 4.1 Windows Raw Input

使用 Windows Raw Input API 获取触控板的原始 HID 输入报告。

核心 API：

- `RegisterRawInputDevices`
- `GetRawInputDeviceList`
- `GetRawInputDeviceInfo`
- `GetRawInputData`
- `WM_INPUT`

Precision Touchpad 的 HID Top-Level Collection：

| 字段 | 数值 |
|---|---|
| Usage Page | `0x0D` |
| Usage | `0x05` |
| Device Class | Digitizers / Touch Pad |

建议注册时使用：

- `RIDEV_INPUTSINK`：允许应用在后台接收输入。
- `RIDEV_DEVNOTIFY`：接收设备连接和断开通知。

通过一个独立的隐藏 Win32 消息窗口接收 `WM_INPUT`。

收到输入后：

1. 获取 RAWINPUT 数据。
2. 确认设备来源。
3. 读取 RAWHID 数据。
4. 解析 HID Input Report。
5. 将结果转换成统一的内部触摸事件。

注意：`RAWHID` 可能包含多个报告，应正确处理 `dwCount` 和 `dwSizeHid`，不能假设每条 WM_INPUT 只包含一个报告。

## 4.2 HID 设备发现

使用：

- `GetRawInputDeviceList`
- `GetRawInputDeviceInfo`
- SetupAPI
- HID API

枚举设备并识别符合 Precision Touchpad 标准的设备。

设备信息至少包括：

- Device Path
- Device Name
- Vendor ID
- Product ID
- Usage Page
- Usage
- HID Report Descriptor
- 最大触点数量
- X/Y Logical Minimum
- X/Y Logical Maximum

如果系统存在多个触控板，应允许用户手动选择。

设备拔出时，应自动释放所有活动触点并停止捕获。

## 4.3 PTP Report Parser

解析 Windows Precision Touchpad 的原始报告。

重点字段：

| 字段 | 用途 |
|---|---|
| Contact ID | 触点唯一标识 |
| Tip Switch | 是否接触触控板 |
| Confidence | 触点置信度 |
| X | 横向坐标 |
| Y | 纵向坐标 |
| Contact Count | 当前触点数量 |
| Scan Time | 可用时用于时间分析 |

不要假设所有触控板具有完全相同的 HID Report Descriptor。

必须根据设备描述符解析字段，正确处理：

- Report ID
- Logical Minimum / Maximum
- Report Size
- Report Count
- 多个 Contact Record
- 不同设备的坐标范围

对于 Confidence 为 false 的触点，可以默认忽略，以减少掌托误触。

解析器必须独立于 Windows API，方便使用录制的 HID 报告进行单元测试。

---

# 5. 触点管理模块

## 5.1 内部数据结构

建议设计统一的数据结构：

```rust
struct TouchContact {
    contact_id: u32,
    x: f64,
    y: f64,
    active: bool,
    confidence: bool,
}

struct TouchFrame {
    timestamp: u64,
    contacts: Vec<TouchContact>,
}
```

实际实现可以根据需要增加接触面积、压力、设备 ID 等字段。

## 5.2 触点生命周期

每个触点必须维护独立状态。

| 状态变化 | 触摸事件 |
|---|---|
| 手指接触触控板 | Down |
| 手指在触控板上移动 | Move |
| 手指离开触控板 | Up |

Contact ID 必须在一次触摸生命周期内保持稳定。

不得因为 HID 报告顺序变化而错误地将两个手指识别为同一个触点。

## 5.3 多指支持

第一版建议至少支持五个同时活动的触点。

实际数量以设备报告的最大触点数为准。

必须支持：

- 两指同时移动。
- 两指捏合和展开。
- 多指同时按下和释放。
- 某一个触点释放时，其余触点继续保持。
- 多个触点在同一输入帧中发生状态变化。

不得将多指数据合并成一个鼠标坐标。

---

# 6. 绝对坐标映射

这是 Pad2Touch 最核心的功能。

## 6.1 坐标转换

将触控板原始坐标转换成归一化坐标：

```text
normalized_x = (raw_x - min_x) / (max_x - min_x)
normalized_y = (raw_y - min_y) / (max_y - min_y)
```

再将其映射到目标矩形：

```text
screen_x = target_left + normalized_x * (target_width - 1)
screen_y = target_top  + normalized_y * (target_height - 1)
```

最终坐标需要取整并限制在目标矩形内。

支持：

- X 轴反转。
- Y 轴反转。
- 90° / 180° / 270° 旋转。
- 自定义触控板输入区域。
- 坐标偏移和缩放。

默认关闭所有额外变换。

## 6.2 目标区域类型

必须支持以下三种模式。

### A. Window Mode

将整个指定窗口的客户区映射到触控板。

通过：

- `GetClientRect`
- `ClientToScreen`
- `GetWindowRect`

获取窗口位置和尺寸。

注意：应使用客户区，而不是包含标题栏和边框的整个窗口外框。

窗口移动或调整大小时，自动更新映射坐标。

窗口最小化、销毁或不可用时，暂停注入并释放活动触点。

### B. Window Rectangle Mode

在指定窗口内部选择一个自定义矩形。

例如：

- 只映射窗口中央区域。
- 只映射安卓模拟器中的手机屏幕。
- 忽略窗口中的工具栏、侧边栏和其他控件。

建议使用相对于窗口客户区的归一化矩形保存配置。

这样窗口缩放后，映射区域可以自动调整。

### C. Screen Rectangle Mode

允许用户在整个桌面上选择任意矩形区域。

通过屏幕覆盖层实现拖动选择。

矩形可以位于：

- 主显示器。
- 副显示器。
- 扩展桌面。
- 不同 DPI 缩放比例的显示器。

必须正确处理负坐标和虚拟桌面坐标。

## 6.3 DPI 处理

进程应启用 Per-Monitor DPI Awareness V2。

坐标映射统一使用 Windows 桌面物理像素。

不能混用逻辑像素和物理像素，否则在 125%、150%、200% 缩放环境下会产生位置偏差。

---

# 7. Windows Touch Injection

## 7.1 核心 API

使用 Windows 原生 Touch Injection。

官方文档：

https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-initializetouchinjection

https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-injecttouchinput

核心 API：

```cpp
InitializeTouchInjection(...)
InjectTouchInput(...)
```

Rust 通过 `windows-rs` 调用。

## 7.2 初始化

应用启动时初始化触摸注入环境。

```cpp
InitializeTouchInjection(
    maxCount,
    TOUCH_FEEDBACK_DEFAULT
);
```

`maxCount` 根据实际设备能力和应用支持上限设置。

第一版建议限制为五个触点。

## 7.3 POINTER_TOUCH_INFO

每个模拟触点都需要建立独立的 `POINTER_TOUCH_INFO`。

重点字段：

| 字段 | 用途 |
|---|---|
| pointerType | PT_TOUCH |
| pointerId | 唯一触点 ID |
| pointerFlags | 当前触摸状态 |
| ptPixelLocation | 屏幕坐标 |
| touchMask | 可选触摸属性 |
| rcContact | 接触区域 |

默认不模拟压力、方向和接触面积，使用系统默认值。

## 7.4 触摸状态

使用以下 Pointer Flags：

| 事件 | Pointer Flags |
|---|---|
| Down | INRANGE + INCONTACT + DOWN |
| Move | INRANGE + INCONTACT + UPDATE |
| Up | INRANGE + UP |
| Cancel | CANCELED 与 UP 或 UPDATE 组合 |

每个触点必须拥有稳定的 `pointerId`。

## 7.5 多触点帧注入

这是必须重点实现的部分。

`InjectTouchInput` 的联系人数组代表桌面上该帧的全部触点，而不是简单地对每个手指分别发送独立鼠标事件。

因此：

- 每帧必须正确维护所有活动触点。
- 同时发生的多个触点变化应在同一帧中提交。
- 一个触点释放时，其他活动触点必须继续保持。
- 触点 ID 在生命周期中不能改变。

不要简单地为每个触点单独调用一次 API。

## 7.6 注入错误处理

必须处理：

- `ERROR_INVALID_PARAMETER`
- `ERROR_NOT_READY`
- API 初始化失败
- 注入过程中设备突然断开
- 目标区域失效
- 显示器分辨率变化

特别注意：

微软文档规定，触点 UP 时的 `ptPixelLocation` 必须与此前最后一次 UPDATE 的位置一致，否则可能导致注入失败，并取消所有活动触点。

因此必须缓存每个触点最后成功注入的位置。

对于 `ERROR_NOT_READY`，应按照 API 文档处理重试，不能直接丢弃状态变化。

长按期间必须持续发送必要的 UPDATE 帧，避免触摸序列被系统取消。

---

# 8. 目标窗口与事件投递

这是项目必须明确理解的系统限制。

`InjectTouchInput` 不提供 HWND 参数。

它将触摸事件注入当前交互桌面，随后由 Windows 根据屏幕坐标进行命中测试，将输入交给相应窗口。

因此 Pad2Touch 的窗口映射实际分为两步：

1. 根据目标窗口或矩形计算屏幕坐标。
2. 由 Windows 将该坐标对应的触摸事件交给最上层窗口。

这意味着：

- 目标窗口必须可见。
- 目标区域不能被其他窗口遮挡。
- 窗口被最小化时必须暂停。
- 目标窗口被其他窗口覆盖时，不能保证事件仍然进入原窗口。

建议实现目标窗口遮挡检测。

可使用 `WindowFromPoint` 等 API 检查映射位置当前的窗口归属。

允许目标窗口的子窗口正常接收事件。

如果目标区域被其他窗口遮挡，应显示警告，必要时暂停注入。

不得通过向目标窗口直接发送 WM_POINTER 消息来冒充 Touch Injection。

---

# 9. 捕获模式与快捷键

Pad2Touch 应提供多种激活方式。

## 9.1 Hold Mode

按住指定快捷键时启用捕获。

例如：

- Alt
- Ctrl + Alt
- Shift + Alt
- 用户自定义组合键

松开快捷键后停止捕获。

## 9.2 Toggle Mode

通过全局快捷键切换捕获状态。

例如：

- Ctrl + Shift + F10

第一次按下启用，再次按下关闭。

## 9.3 Always-on Mode

应用运行期间持续启用捕获，直到用户手动关闭。

## 9.4 Manual Mode

通过主界面或系统托盘手动开启和关闭。

## 9.5 多种激活方式

允许用户配置多种激活方式。

建议：

- 使用 `RegisterHotKey` 实现 Toggle。
- 使用 Raw Input Keyboard 或其他合适的用户态输入方式跟踪 Hold 状态。
- 不应无必要地拦截键盘事件。
- 不应影响其他应用正常使用快捷键。

提供一个全局紧急停止快捷键。

任何激活方式被解除时，都必须释放所有模拟触点。

## 9.6 捕获状态机

建议设计：

```text
DISABLED
    │
    ▼
ARMING
    │
    │ 等待所有手指离开触控板
    ▼
ACTIVE
    │
    │ 快捷键释放 / 手动停止
    ▼
DISARMING
    │
    │ 释放所有活动触点
    ▼
DISABLED
```

激活时如果用户已经将手指放在触控板上，应先等待全部触点释放，再开始新的捕获周期，避免产生意外触摸。

---

# 10. 原生鼠标事件与冲突处理

这是 Windows 实现中必须公开说明的限制。

Windows Precision Touchpad 的原始触摸报告会被系统同时用于生成传统鼠标操作。

Raw Input 读取不会独占设备。

Tapview 的 Windows 实现也存在这一限制，不能像 Linux 那样通过独占设备读取来停止系统鼠标行为。

因此捕获期间可能同时出现：

- 正常鼠标指针移动。
- 系统触控板点击。
- 系统滚动。
- Pad2Touch 注入的触摸事件。

## 10.1 MVP 处理策略

第一版不尝试强行屏蔽系统鼠标事件。

优先保证真实触摸注入的稳定性。

UI 中应说明捕获期间原生触控板鼠标行为可能继续存在。

## 10.2 后续研究方向

可以研究 `WH_MOUSE_LL` 低级鼠标钩子。

但它存在重要问题：

- 无法可靠地区分内置触控板和外接鼠标产生的普通鼠标事件。
- 可能连外接鼠标也被屏蔽。
- 全局拦截可能影响其他应用。
- 可能与游戏、远程桌面及其他输入工具发生冲突。

因此不得将全局鼠标拦截作为 MVP 的默认行为。

只有在找到可靠的设备级隔离方案后，才考虑实现独立的输入抑制功能。

---

# 11. 用户界面

使用 egui / eframe。

整体风格应简洁、现代，优先考虑 Windows 原生工具的使用体验。

## 11.1 主界面

包含：

- 当前设备名称。
- 设备连接状态。
- 捕获状态。
- 当前目标窗口。
- 目标区域预览。
- 当前活动触点数量。
- 快捷键状态。

## 11.2 Device Settings

支持：

- 自动发现设备。
- 手动选择设备。
- 显示 Vendor ID / Product ID。
- 显示最大触点数量。
- 显示坐标范围。
- 显示报告频率。

## 11.3 Target Settings

支持：

- 选择目标窗口。
- 选择窗口客户区。
- 自定义窗口内矩形。
- 桌面矩形选择器。
- 实时预览映射区域。
- 窗口移动和缩放时自动更新。

## 11.4 Mapping Settings

支持：

- X/Y 轴反转。
- 旋转。
- 自定义触控板活动区域。
- 映射区域边距。
- 坐标校准。
- 可选平滑处理。

默认不进行坐标平滑，优先保证低延迟。

## 11.5 Activation Settings

支持：

- Hold。
- Toggle。
- Always-on。
- Manual。
- 自定义快捷键。
- 紧急停止快捷键。

## 11.6 Diagnostics

建议提供实时调试面板：

- 原始触点数量。
- 当前活动触点。
- 触点 ID。
- 原始 X/Y。
- 映射后 X/Y。
- 注入成功/失败次数。
- 当前输入报告频率。
- 最近一次 Windows API 错误。

---

# 12. 软件架构与目录建议

建议采用模块化结构：

```text
pad2touch/
│
├── Cargo.toml
├── README.md
├── LICENSE
│
├── src/
│   ├── main.rs
│   ├── app.rs
│   ├── config.rs
│   ├── state.rs
│   │
│   ├── input/
│   │   ├── mod.rs
│   │   ├── contact.rs
│   │   ├── contact_manager.rs
│   │   └── frame.rs
│   │
│   ├── platform/
│   │   └── windows/
│   │       ├── mod.rs
│   │       ├── raw_input.rs
│   │       ├── device_discovery.rs
│   │       ├── hid_parser.rs
│   │       ├── touch_injection.rs
│   │       ├── window_target.rs
│   │       ├── keyboard.rs
│   │       └── display.rs
│   │
│   ├── mapping/
│   │   ├── mod.rs
│   │   ├── coordinate.rs
│   │   ├── transform.rs
│   │   └── target_region.rs
│   │
│   ├── capture/
│   │   ├── mod.rs
│   │   ├── controller.rs
│   │   ├── state_machine.rs
│   │   └── hotkeys.rs
│   │
│   ├── ui/
│   │   ├── mod.rs
│   │   ├── device_panel.rs
│   │   ├── target_panel.rs
│   │   ├── mapping_panel.rs
│   │   ├── activation_panel.rs
│   │   └── diagnostics.rs
│   │
│   └── logging.rs
│
├── tests/
│   ├── mapping_tests.rs
│   ├── contact_tests.rs
│   ├── hid_parser_tests.rs
│   └── injection_tests.rs
│
└── docs/
    ├── architecture.md
    ├── troubleshooting.md
    └── development.md
```

允许根据实际开发需要调整，但必须保持输入采集、坐标映射、触点管理和注入模块相互独立。

---

# 13. 性能与稳定性要求

Pad2Touch 是实时输入工具，延迟和输入连续性非常重要。

## 13.1 性能目标

建议目标：

| 项目 | 目标 |
|---|---|
| 输入报告处理 | 不阻塞 UI |
| 触摸注入 | 尽可能与原始报告同步 |
| 额外处理延迟 | P95 小于 10 ms |
| 空闲 CPU | 尽可能低于 1% |
| 活动内存 | 尽可能低于 100 MB |
| 多指输入 | 至少 5 指 |
| 输入状态 | 不允许出现永久卡住的触点 |

以上是开发目标，不是未经测试即可宣称的性能保证。

## 13.2 线程设计

建议：

- Raw Input：独立消息线程。
- HID Parser：轻量处理，可与采集线程合并。
- Contact Manager：维护状态。
- Coordinate Mapper：纯计算模块。
- Touch Injection：独立注入线程或串行执行器。
- UI：独立线程。

所有触摸帧应按顺序处理。

不允许因为 UI 刷新或日志写入造成触摸帧积压。

高频输入可以采用有界队列，但不得随意丢弃 Down / Up 状态转换。

---

# 14. 开发阶段与任务拆分

**2026-11-17 开发顺序调整：** 优先交付 Phase 4 中的 GUI 和基础捕获管理，将现有诊断接收窗口整合进主界面，提供设备选择、手动启停、Toggle / Hold 快捷键，并预留 Phase 3 的映射目标接口。当前映射仍为诊断区域的固定矩形；这不代表 Phase 2 实机验收、Phase 3 或完整 Phase 4 已完成。配置持久化、Always-on 等后续项目仍保留。

**2026-11-17 Phase 3 实现：** Window Mode、Window Rectangle Mode（客户区归一化矩形）与 Screen Rectangle Mode（覆盖层拖动选择，支持负坐标）已在 GUI 中实现；会话期间跟随窗口移动/缩放更新映射，最小化/隐藏时暂停并释放触点，窗口销毁时停止；注入前按命中测试检测遮挡。已有自动化合成测试覆盖；使用实体触控板对第三方应用及 125%/150%/200% 缩放的人工验收仍待进行。

**必须遵循先验证核心技术，再开发完整 UI 的顺序。**

## Phase 0：技术可行性验证

这是整个项目最重要的阶段。

暂时不开发完整 GUI。

实现一个最小 Rust 程序：

1. 发现当前 Windows Precision Touchpad。
2. 使用 Raw Input 接收触摸报告。
3. 解析单指 Contact ID、Tip Switch、X/Y。
4. 使用 `InitializeTouchInjection` 初始化。
5. 将触控板坐标映射到固定屏幕矩形。
6. 使用 `InjectTouchInput` 注入真实触摸事件。

编写一个简单的 Windows 触摸测试窗口，验证其是否能收到：

- Pointer Down。
- Pointer Update。
- Pointer Up。

**验收条件：**

手指在触控板上的移动能够直接转化为屏幕矩形中的真实触摸轨迹。

如果这一阶段无法完成，必须先查明具体原因，不得直接改用鼠标模拟作为替代。

## Phase 1：单指 MVP

实现：

- 设备发现。
- Raw Input。
- HID Parser。
- 单指状态管理。
- 绝对坐标映射。
- 触摸注入。
- 固定屏幕矩形。

完成最基础的端到端流程。

## Phase 2：多指支持

实现：

- 多 Contact ID。
- 多指同步注入。
- 多指生命周期管理。
- 两指缩放。
- 同时按下和释放。
- 错误恢复。

## Phase 3：窗口与矩形映射

实现：

- Window Mode。
- Window Rectangle Mode。
- Screen Rectangle Mode。
- 窗口移动和缩放更新。
- DPI 处理。
- 窗口遮挡检测。

## Phase 4：捕获管理与 GUI

实现：

- Hold。
- Toggle。
- Always-on。
- Manual。
- 自定义快捷键。
- 紧急停止。
- 配置持久化。
- 设备管理界面。
- 诊断界面。

## Phase 5：可靠性与发布

实现：

- 设备热插拔。
- 显示器变化处理。
- 注入错误恢复。
- 捕获中断恢复。
- 系统托盘。
- 便携版发布。
- 安装包。

---

# 15. 测试与验收标准

## 15.1 输入测试

至少测试：

- 单指轻点。
- 单指长按。
- 单指拖动。
- 两指同时移动。
- 两指捏合。
- 三指及以上操作。
- 多指同时释放。
- 快速连续点击。

## 15.2 坐标测试

测试：

- 映射区域四角。
- 映射区域中心。
- X/Y 轴反转。
- 不同矩形尺寸。
- 窗口移动。
- 窗口缩放。
- 多显示器。
- 负屏幕坐标。
- 125%、150%、200% DPI 缩放。

## 15.3 稳定性测试

测试：

- 捕获过程中切换窗口。
- 捕获过程中关闭目标窗口。
- 捕获过程中拔出触控板。
- 捕获过程中切换显示器分辨率。
- 快捷键异常释放。
- 应用退出时释放全部触点。
- API 注入失败后恢复。
- 长时间持续拖动。

## 15.4 兼容性测试

至少覆盖：

- Windows 10。
- Windows 11。
- 不同厂商的 Precision Touchpad。
- 不同触控板最大触点数量。
- 内置触控板。
- 外接 Precision Touchpad（如可用）。

## 15.5 发布验收

必须满足：

1. 不需要安装驱动。
2. 不需要管理员权限完成基本功能。
3. 不修改目标应用。
4. 不注入目标进程。
5. 不产生鼠标模拟触摸的假实现。
6. 单指及多指输入正常。
7. 坐标映射正确。
8. 捕获停止后不存在卡住的触点。
9. 正常模式下不修改系统触控板设置。
10. 对尚未解决的原生鼠标事件冲突有明确说明。

---

# 16. 安全与异常处理

所有异常情况都应优先保证触点释放。

以下情况必须触发安全释放：

- 用户关闭捕获。
- 紧急停止快捷键触发。
- 目标窗口销毁。
- 目标窗口最小化。
- 触控板断开。
- 显示器配置变化。
- 注入模块发生不可恢复错误。
- 应用准备退出。

不得让触点长期停留在 DOWN 状态。

默认不记录键盘的普通输入内容。快捷键模块仅处理用户配置的激活组合。

不应包含遥测、远程输入传输或未经授权的数据收集。

---

# 17. Agent 开发约束

Coding Agent 必须遵循以下规则。

**技术约束：**

- 优先使用 Windows 官方 Touch Injection API。
- 优先研究 Tapview 的 Windows Raw Input 实现。
- 使用标准 HID / PTP 数据格式。
- 使用绝对坐标映射。
- 真实处理多触点。
- 保证所有输入状态转换完整。

**禁止事项：**

- 不得使用 SendInput 模拟触摸。
- 不得将鼠标点击转换作为主要实现。
- 不得向目标 EXE 注入 DLL。
- 不得修改其他进程内存。
- 不得要求用户安装内核驱动。
- 不得使用 Android 模拟器专用接口。
- 不得为了实现捕获而修改触控板固件。
- 不得未经验证就实现全局鼠标事件拦截。

**开发要求：**

- 先完成 Phase 0，再继续开发。
- 每个阶段完成后提供可运行版本。
- 所有 Windows API 调用必须进行错误处理。
- 核心模块应具有独立测试。
- 关键设计决定记录在 `docs/architecture.md`。
- 不得用未经验证的假数据宣称硬件兼容。
- 遇到 API 限制时，先报告问题及实验结果，不得擅自改变项目目标。

---

# 18. 参考资料

## Microsoft 官方文档

1. [Windows Precision Touchpad Collection](https://learn.microsoft.com/en-us/windows-hardware/design/component-guidelines/touchpad-windows-precision-touchpad-collection)

2. [Precision Touchpad Input](https://learn.microsoft.com/en-us/windows/win32/input-precisiontouchpad/precision-touchpad-portal)

3. [Raw Input Overview](https://learn.microsoft.com/en-us/windows/win32/inputdev/about-raw-input)

4. [Using Raw Input](https://learn.microsoft.com/en-us/windows/win32/inputdev/using-raw-input)

5. [WM_INPUT](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-input)

6. [InitializeTouchInjection](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-initializetouchinjection)

7. [InjectTouchInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-injecttouchinput)

8. [POINTER_TOUCH_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-pointer_touch_info)

## 技术参考项目

- [Tapview](https://github.com/JohnAZoidberg/tapview)

---

# 19. 项目最终愿景

Pad2Touch 不应只是一个将触控板映射为鼠标的工具。

它的核心价值在于：

**将普通 Windows Precision Touchpad 转换为真正的多点触摸输入设备。**

用户无需触摸屏，也无需修改目标应用，就可以通过笔记本触控板直接操作任何能够接收 Windows 触摸输入的应用。

项目应保持轻量、独立、开源，并尽可能采用 Windows 原生 API。

最终目标是实现一套接近 macOS Touch Alternatives / Trackpad Capture 使用体验的 Windows 方案。

**Pad2Touch — Bridge your touchpad to touchscreen input.**
