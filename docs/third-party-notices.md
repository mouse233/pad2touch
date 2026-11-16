# 第三方声明

PadBridge 原创代码及随附文档采用仓库根目录的 MIT 许可证。该声明不替代第三方组件的许可证。

## Rust 依赖

`Cargo.lock` 固定以下 windows-rs 项目组件：

| 组件 | 版本 | 上游声明许可证 | 上游 |
|---|---|---|---|
| windows-sys | 0.61.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-link | 0.2.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |

组件的版权归其上游权利人。通过 Cargo 获取的 crate 保留其 `license-mit`、`license-apache` 及元数据；分发包含这些组件的构建产物时，也应随附所选第三方许可证和适用通知。对应许可证可见上游 [MIT](https://github.com/microsoft/windows-rs/blob/master/license-mit) 与 [Apache-2.0](https://github.com/microsoft/windows-rs/blob/master/license-apache)。

## 技术参考

Tapview 仅作为 Windows Raw Input/HID API 路线参考，未复制、修改或分发其源码，PadBridge 的 MIT 许可证不对 Tapview 作任何授权声明。Microsoft Learn 和官方示例链接为技术参考，具体链接见 `architecture.md`。
