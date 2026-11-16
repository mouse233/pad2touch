# Third-party notices

PadBridge's original code and accompanying documentation use the MIT License in the repository root. That license does not replace the licenses of third-party components.

## Rust dependencies

`Cargo.lock` pins these components from the windows-rs project:

| Component | Version | Declared license | Upstream |
|---|---|---|---|
| windows-sys | 0.61.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-link | 0.2.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |

Copyright remains with the respective upstream rights holders. Crates obtained through Cargo retain their license files and metadata. Distributions containing these components should include the selected third-party license and applicable notices. The upstream licenses are available as [MIT](https://github.com/microsoft/windows-rs/blob/master/license-mit) and [Apache-2.0](https://github.com/microsoft/windows-rs/blob/master/license-apache).

## Technical references

Tapview was consulted only as a reference for the Windows Raw Input and HID API approach. Its source code was not copied, modified, or redistributed. PadBridge's MIT License does not grant rights to Tapview.

Microsoft documentation and official samples are technical references. See [architecture](architecture.md) for the relevant links.
