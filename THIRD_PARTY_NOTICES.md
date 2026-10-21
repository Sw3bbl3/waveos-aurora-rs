# Third-party components

WaveOS's native media integration uses these vendored decoder components:

| Component | Version | License | Local changes |
|---|---|---|---|
| rusty_h264-decoder | 0.16.0 | [BSD-2-Clause](vendor/rusty_h264-decoder/LICENSE) | [no_std imports and formatting](vendor/rusty_h264-decoder/WAVEOS-PATCHES.md) |
| rusty_aac | 1.1.0 | [Apache-2.0](vendor/rusty_aac/LICENSE) | [decoder-only no_std port and formatting](vendor/rusty_aac/WAVEOS-PATCHES.md) |

The bootable system includes these license texts under `/System/Licenses`. Cargo manifests and lockfiles identify registry dependencies, including `rusty_h264-common` 0.16.0 and the `regress` regular-expression engine. Their upstream copyright and licensing remain applicable.

Media test fixtures have separate provenance and terms, documented in [the fixture README](libs/media/tests/fixtures/README.md). The MP4 sample is CC0; the AAC tone is generated test data. Downloaded third-party website scripts used during development are not redistributed in release assets.
