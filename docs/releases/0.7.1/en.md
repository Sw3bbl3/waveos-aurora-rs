# WaveOS Aurora 0.7.1

Not yet published.

This update makes WaveOS sturdier with data it didn't create. Damaged pictures, fonts, cookies, USB sticks and disks are now refused cleanly instead of closing an app or stopping the system, and the whole codebase passes a stricter automated review.

## Preview, Files and Nebula
- Damaged JPEG pictures no longer close Preview, Files or Nebula.
- Nebula no longer closes when a website sends a cookie whose date contains accented or other non-English letters.
- Pages with absurdly large CSS sizes, such as `margin: 1e40px`, lay out instead of producing wrapped-around positions.
- A font with a malformed glyph outline no longer crashes text drawing.

## Storage
- A damaged FAT32 USB stick or AuroraFS volume is reported as damaged instead of stopping the system, and checking a damaged AuroraFS volume no longer hangs.
- Asking to grow a file beyond the free space on its disk is refused at once, instead of exhausting system memory.

## Sound
- Loud recordings no longer click when they're played at a different sample rate.

## Notes
- The Find button's magnifying glass no longer shows a dark disc when the pointer is over it.

## For developers
- `cargo xtask lint` runs Clippy on the host libraries, kernel, bootloader and user space, with warnings as errors, and CI runs it on every change.
- Every parser of outside data has a robustness test that feeds it thousands of corrupted inputs. See [Building and testing](../../handbook/0.7/en/developer-build.md).
- The kernel now always uses the portable SHA-256 code, as it must: it never saves SIMD registers.

## Known limitations
Robustness tests mutate known-good inputs at random; they are not coverage-guided fuzzing, so they make further crash bugs less likely but cannot rule them out. Read the [complete handbook](../../handbook/0.7/en/start-welcome.md).

## Evidence
`cargo xtask lint`, the host test suite in [Building and testing](../../handbook/0.7/en/developer-build.md), and the kernel self-tests (`cargo xtask test`) were run on the change. Each fixed crash has a regression test that fails without its fix.
