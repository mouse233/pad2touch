# Optional local recording tests

Physical touchpad recordings and Windows HID preparsed data are local diagnostic materials. `.gitignore` excludes them from the repository; only the test logic and this documentation are tracked.

A normal checkout can build and run `cargo test --locked` without any recordings. Two recording tests are ignored by default, and their inputs are read at runtime rather than embedded during compilation.

On a machine retaining the expected private fixture set:

```powershell
cargo test --locked -- --include-ignored
```

The optional tests expect these local files:

- `asue1a03-preparsed.bin`: an aligned `RIDI_PREPARSEDDATA` snapshot matching the test layout. It is opaque Windows HID parser data, **not** the original HID report descriptor. Portability across Windows versions is not guaranteed.
- `five-contacts.hex`: one frame of five concurrent contact reports, one hexadecimal report per line. The first report has Contact Count 5, the others have count zero, and all share Scan Time.

The tests run the fixture through HidP_GetCaps, GetValueCaps, GetButtonCaps, and GetData. They check contact IDs, Tip Switch, Confidence, logical coordinate ranges, assembly, and rejection of short input.

Fixture replay does not establish live hardware compatibility. A different device requires its own reviewed layout and expected values. Do not add private recordings, raw device paths, or local test results to this directory's tracked files.
