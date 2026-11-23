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
- Logical comparisons (`do_logical_op`) compare strings and buffers as
  whole byte strings (ACPI 6.5, 19.6.70 and following). The crate compared
  the first four bytes and panicked on longer buffers, so every `_DSM`
  (`If (Arg0 == ToUUID (...))`) stopped the ACPI task.
- `ConcatenateResTemplate` drops both templates' end tags and appends one
  (`0x79 0x00`). The crate kept both and appended `0x78`.
- Field lists: `AccessAs` and extended `AccessAs` set the access type of the
  fields that follow, and `Connection (...)` (GPIO and serial-bus regions)
  is skipped. All three were `todo!()`, which lost ACPI on laptops whose
  tables have such fields; accessing a field of a region we have no
  handler for returns an error.
