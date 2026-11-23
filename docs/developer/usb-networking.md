# USB networking (`kernel/src/drivers/usb/net`, `libs/usbnet`)

For engineers working on USB network adapters: Ethernet dongles, USB-C docks, phone tethering. Which devices are driven, and how far each is tested, is in [HARDWARE.md](../HARDWARE.md#usb-network-adapters).

## Layout

| Where | What |
|---|---|
| `libs/usbnet` (`aurora-usbnet`, `no_std`, host-tested and fuzzed) | Everything that parses what a device sends or builds what it receives: CDC functional descriptors and notifications (`cdc`), NCM transfer blocks and their parameters (`ncm`), EEM framing (`eem`), RNDIS control and data messages (`rndis`), ASIX register names, MII helpers and framing (`asix`), configuration choice and the vendor-chip id table (`select`) |
| `kernel/src/drivers/usb/net/mod.rs` | `UsbNic` (the `net::Nic`), its receive path, transmit ring and task, `start` (registering an adapter), `choose` (configuration), the System Report lines |
| `kernel/src/drivers/usb/net/cdc.rs` | Bring-up of ECM, NCM and EEM functions; CDC notifications |
| `kernel/src/drivers/usb/net/rndis.rs` | RNDIS bring-up over encapsulated commands; indications and keepalives later |
| `kernel/src/drivers/usb/net/asix.rs` | AX88772 and AX88179 bring-up, link handling |
| `kernel/src/drivers/usb/net/sim.rs` (self-test builds) | A simulated host controller with NCM, AX88179 and AX88772 device models |
| `kernel/src/net/mod.rs` | `register`/`unregister`, naming, `Nic::metric` and the route choice, `Nic::speed_mbps` |
| `libs/usbnet/tests/hostile.rs` | Every parser fed corrupted inputs (`aurora-mutate`) |

Anything a device says is untrusted: lengths, offsets, counts and descriptor fields are checked in `aurora-usbnet` before they index anything, every frame handed to the stack is between 14 and 1518 bytes, and the receivers that carry bytes across transfers (RNDIS, EEM, AX88772) bound what they keep. Add parsing there, not in the kernel, and add the format to `hostile.rs`.

## How a device comes up

1. The `usb` task enumerates it. With more than one configuration, it reads them all and `net::choose` picks (`select::configuration`): NCM 50, ECM 40, a vendor chip with a driver here 35, EEM 30, RNDIS 20, other drivable classes 10; the best wins, ties go to the first; the `usbnet=` boot option scores its function 100 where offered. This is what puts Realtek docks in their CDC configuration and QEMU's usb-net in ECM.
2. `usb::attach` offers each interface to the class drivers: a communication interface whose class triple is a `Function` goes to `net::attach`; a vendor-class interface of a chip in `select::CHIPS` to `net::attach_chip`.
3. The driver's `bring_up` returns a `Setup2`: label, framing, endpoints, MAC address, initial link and speed, buffer sizes. It does the device-specific talking: for ECM and NCM the MAC string, NTB parameters and input size, then `iso::set_interface` to the data interface's setting with the bulk pair; for RNDIS INITIALIZE, queries and the packet filter; for the ASIX chips their reset sequences.
4. `net::start` builds the `UsbNic`, registers it with `net` (which names it and starts DHCP once the link is up), opens the receive stream and the interrupt pipe, starts the adapter's `usbnet` task, and installs the detach hook.

## Data path

- **Receive.** `Bus::open_stream` keeps 4–16 transfers queued on the bulk IN endpoint (64 KiB in flight). On xHCI each completion runs `UsbNic::received` from the `xhci-events` task with a slice of the DMA buffer itself; the framing splits it into frames, each copied once into the `Vec` `net::receive` takes. Controllers that can't stream (EHCI, OHCI, UHCI) get a `usbnet-rx` task looping over `Bus::transfer` with a five-second timeout (EHCI, OHCI and UHCI take a timed-out transfer back cleanly; xHCI now does too). A stream whose endpoint halts is reset and refilled by the controller (see ARCHITECTURE.md), so a transaction error on a real bus doesn't end reception.
- **Transmit.** `Nic::send` runs under the stack's lock, so it never waits: it packs the frame straight into the buffer being filled in a ring of DMA buffers (`Tx`; 128 one-frame buffers for ECM, RNDIS-with-one-packet and ASIX, 8 aggregating ones for NCM, EEM and multi-packet RNDIS, carved out of 64 KiB regions), and returns false (the frame is dropped and counted) only when the ring is full. The `usbnet` task hands every ready buffer to the controller in one `Bus::transfer_batch` (xHCI: one TD each, one doorbell, one completion to wait for), adding a zero-length packet where a transfer fills whole packets. While a batch is on the bus, frames collect in the next buffers: under load, NCM blocks and RNDIS transfers fill up and batches grow; idle, a lone frame goes out at once.
- **Status.** Interrupt-endpoint reports are copied for the task (the pipe handler runs on the controller's event task and must not block); the task applies CDC notifications, fetches RNDIS responses (indications, the device's keepalives), or reads the PHY on an ASIX link change. Link changes go to `net::link_changed`, which restarts DHCP.
- **Unplug.** The detach hook sets `gone`, wakes the task and calls `net::unregister`: `update_link(false)` fails TCP connections from the interface's addresses (`tcp::network_lost`), ARP, neighbor and pending packets go, the slot is marked gone (its index stays valid; `register` reuses it, so a replugged adapter is `usb0` again) and the receive queue is purged of its frames. The controller drops the streams and pipes when it releases the device, the task ends, and with the last `Arc` the DMA buffers are freed (the QEMU test checks this with a `Weak`).

## Adding a vendor chip

1. Its ids go in `select::CHIPS` with a new `Chip`, and its register names and framing in a module of `aurora-usbnet` with host tests (a round trip, and the receive side in `hostile.rs`).
2. A `Framing` variant, its arms in `UsbNic::received`, `pack` and `transmit` (does the chip want a zero-length packet or padding?), and `bring_up`/`status`/`tick` like `asix.rs`.
3. If the chip also offers a CDC configuration, decide which is safer: the class driver is usually the better-tested path; a vendor driver adds what CDC lacks (offloads, aggregation, link speed).
4. A model in `sim.rs` and a line in the self-test, so at least the plumbing runs in `cargo xtask test`; and a row in HARDWARE.md marked untested until someone runs it on the device.

## Testing

| Command | What runs |
|---|---|
| `cargo test -p aurora-usbnet` | Formats and fuzzing (`AURORA_MUTATIONS=20` for a longer search) |
| `cargo xtask test --net usb-ecm` (or `usb-rndis`) | The whole suite with QEMU's usb-net as the network: two adapters, the hot-unplug and replug over QMP, throughput |
| `cargo xtask test --net usb-ecm --usb ehci` | The same on an ICH9 EHCI with UHCI companions (usb-net is full speed, so it runs on a companion) |
| any `cargo xtask test` | The simulated NCM, AX88179 and AX88772 adapters (`AURORA_TEST_FILTER="USB network"` for just these) |
| `cargo xtask run --net usb-ecm` | The desktop on a USB adapter; `usbnet0` can be unplugged from the QEMU monitor (`device_del usbnet0`) |

The network test prints the time and CPU time for a megabyte each way to the host (`net-test: 1 MiB from the host in …`); the USB test prints what two idle adapters cost and each adapter's frames per transfer and batches (`dmesg | grep net-test`, or the System Report).

## Measurements

QEMU 11 under TCG on an 8-core Mac shared with other work (load averages of 20–70 during these runs), 4 guest CPUs; two or three runs each, so treat differences under 2× as noise. "CPU ms" is busy time summed over all CPUs while the transfer ran. QEMU's usb-net moves one Ethernet frame per USB transfer and holds one received frame at a time, so receive aggregation can't show here, and it doesn't enforce USB bus speeds; the NCM and ASIX aggregation paths run only against the simulated devices.

| Network | 1 MiB from the host | 1 MiB to the host |
|---|---|---|
| virtio-net | 29–35 ms, 47–68 CPU ms | 47–68 ms, 65–115 CPU ms |
| e1000 | 560–590 ms, 72–80 CPU ms | 270–280 ms, 65–70 CPU ms |
| usb-net CDC-ECM, xHCI | 29–47 ms, 88–111 CPU ms | 31–65 ms, 87–187 CPU ms |
| usb-net RNDIS, xHCI | 19–69 ms, 61–148 CPU ms | 25–47 ms, 70–142 CPU ms |
| usb-net CDC-ECM, xHCI, before batched transmits | 44–89 ms, 154–204 CPU ms | 173–280 ms, 656–888 CPU ms |
| usb-net CDC-ECM, EHCI (UHCI companion) | about 2 s, 1.8–2.3 CPU s | about 1.9 s, 1.5–1.9 CPU s |

Two idle usb-net adapters cost 38–39 CPU ms per second in ECM mode (QEMU answers every poll of the notification endpoint) and 18–20 in RNDIS mode, on xHCI; on EHCI's UHCI companion, in ECM mode, 400–540 CPU ms per second (see below). Unplugged with QMP `device_del`, an adapter's interface is gone 2–7 ms later on xHCI (90–175 ms on UHCI), and a connection through it fails at the same moment; plugged back with `device_add`, the interface is there 15–70 ms later and has its DHCP address another 70 ms to 1.3 s later (QEMU's DHCP server answers at its own pace).

A bug this work found: the xHCI driver handed event-ring entries back only after draining everything, so a busy receive stream plus transmits could fill the 256-entry ring, and a full ring makes the controller (QEMU included) drop events. Each dropped receive completion was a stream buffer never queued again; with RNDIS's four buffers, reception died after a few losses (the adapter's report showed receive silent for 40 s while transmits went on, with one transmit timeout). The driver now hands entries back every 16 events, the ring has 1024 entries, and a reported overflow restarts every pipe.

Where the time goes:

- **Receive on xHCI** is close to virtio: frames are split in place on the event task. The extra CPU is the per-frame transfer event and wake-up (QEMU completes one frame per TD).
- **Transmit** was one transfer per frame with a wait in between, and a ring of 64 buffers: a TCP burst overflowed it and the drops cost retransmission timeouts (an RNDIS upload once took 1.2 s with 6 frames dropped). The ring is now 128 buffers and every ready buffer goes to the controller in one batch (about 4–7 frames per batch during the upload), at a fifth of the CPU time.
- **UHCI** (full speed through an EHCI's companion) can't stream: one transfer at a time with the schedule walked once a millisecond, about 0.5 MB/s. Its interrupt pipes are polled every frame whatever the endpoint's interval, and QEMU's usb-net answers every poll in ECM mode (real devices answer only when something changed), which keeps the controller's event task busy: two idle ECM adapters there cost 400–540 CPU ms per second. Honouring the endpoint's interval in the UHCI driver would fix that (and is worth doing for every interrupt pipe on UHCI). The receive task's transfer used to look for a lost interrupt every 20 ms while waiting, fifty wake-ups a second per adapter, enough to fail the quiet-idle test; it now backs off to 250 ms.
