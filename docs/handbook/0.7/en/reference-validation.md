# Validation and performance evidence
Release validation distinguishes automated checks, observed UI interactions, measurements, and untested configurations.

## Read the results
1. Open the release’s validation report from the GitHub release assets.
2. Match the reported commit, image checksum, host, emulator, resolution, and workload to your configuration.
3. Compare equivalent metrics: composition/flush time is not end-to-end input latency, and pixels redrawn are not elapsed time.

Desktop measurements use one warm-up and five measured repetitions. Learn is new in 0.7, so startup/search/scroll results are an initial baseline rather than a claimed improvement over a nonexistent older version. Numerical gains appear only when repeatable measurements support them.

## Reproduce
Use `tools/benchmark_desktop.py LOG OUTPUT.json` against the documented VM configuration. Preserve raw frame samples and calculate median and p95 by workload. A development-baseline comparison must be labeled as such rather than described as a previous public release.

## Troubleshooting
If a report lacks a test for your hardware, treat support as unverified. Do not extrapolate a best-case percentage to the whole operating system.
