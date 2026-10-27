Vendored from acpi 6.1.1 (MIT OR Apache-2.0).

WaveOS changes, in `src/aml/mod.rs` (`do_store`):

- A store through a reference made by `Index` or `RefOf` writes the
  referenced object: a package element is replaced, a buffer or string byte
  is written as a buffer field, a field unit goes through a field write.
  The crate panicked here (`todo!()`), so firmware that fills its `_BST`
  package with `Store (x, Index (PBST, 2))`, as many laptops do, stopped
  the ACPI task and with it battery reporting.
- A store to an unresolved reference returns an error instead of panicking.
- `src/lib.rs` allows `stable_features` (`allocator_api` is partly stable on the pinned nightly).
- PCI configuration regions (`do_native_region_read`/`write`) add the
  region's base to the field offset, as system memory and I/O regions do.
  The crate read offset 0 for every `OperationRegion (X, PCI_Config, 0x18, …)`.
