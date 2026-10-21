# Known limitations
WaveOS 0.7 is a development operating system. A release number does not certify production readiness or universal hardware compatibility.

## Current boundaries
- No native Rust/Cargo/LLVM toolchain, semantic Studio IDE services, or source debugger.
- JavaScript and web-platform compatibility are partial. JavaScript modules, BigInt, Intl, SVG, and WebP are unavailable; complex sites and protected streaming services can fail.
- No Wi-Fi, I²C touchpads, native GPU driver, or Modern Standby. IPv6 provides neighbor discovery, SLAAC, ICMPv6, TCP and UDP, with QEMU validation rather than physical-network certification.
- Native MP4/H.264 and AAC-LC playback is experimental; DRM and general YouTube/TikTok compatibility are not established.
- GINA has structural validation but no package signatures or app-data permission isolation.
- The desktop and other apps retain English interfaces; Learn and its guides support English and Canadian French.
- ISO sessions need a writable compatible volume for persistent home files.

## Check before relying on a feature
1. Find the feature’s guide and read its prerequisites.
2. Check the validation report for an executed test on the relevant configuration.
3. Keep backups and perform a small reversible trial before using important data.

Reported QEMU performance describes the tested emulator configuration. It does not establish native-hardware frame rate, host energy efficiency, or physical keyboard capture.
