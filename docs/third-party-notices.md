# Third-party notices

Pad2Touch's original code and accompanying documentation use the MIT License in the repository root. That license does not replace the licenses of third-party components.

## Rust dependencies

`Cargo.lock` pins these components from the windows-rs project:

| Component | Version | Declared license | Upstream |
|---|---|---|---|
| windows-sys | 0.61.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-link | 0.2.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |

Copyright remains with the respective upstream rights holders. Crates obtained through Cargo retain their license files and metadata. Distributions containing these components should include the selected third-party license and applicable notices. The upstream licenses are available as [MIT](https://github.com/microsoft/windows-rs/blob/master/license-mit) and [Apache-2.0](https://github.com/microsoft/windows-rs/blob/master/license-apache).

## Technical references

Tapview was consulted only as a reference for the Windows Raw Input and HID API approach. Its source code was not copied, modified, or redistributed. Pad2Touch's MIT License does not grant rights to Tapview.

Microsoft documentation and official samples are technical references. See [architecture](architecture.md) for the relevant links.

The isolated settings-cache experiment uses interface facts reported by [co_init_ex on Stack Overflow](https://stackoverflow.com/questions/73331905/disable-gestures-through-registry) and discussed by [gexgd0419 in the NVDA project](https://github.com/nvaccess/nvda/discussions/19841). Pad2Touch's experiment is independently written; no Stack Overflow code block, NVDA source module, or external script is embedded. These sources do not grant a Microsoft compatibility guarantee for the undocumented interface. See the [research and license boundary](background-gesture-feasibility.md#reference-and-license-boundary).
