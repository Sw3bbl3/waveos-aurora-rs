Vendored from rusty_aac 1.1.0 (Apache-2.0).

WaveOS decoder-only no_std port: alloc collection imports, core namespace,
spin::Once for lazily initialized tables, num-traits/libm floating point
operations. Encoder, profiling, laboratory and runtime SIMD detection require
the std feature. Decoder algorithms are unchanged.
Silence unused-code warnings in the decoder-only build (lib.rs).

Sources are normalized with the repository rustfmt configuration.
