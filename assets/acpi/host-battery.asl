/*
 * A laptop battery and power adapter for QEMU that mirror the host's.
 *
 * `cargo xtask run --battery` attaches an ivshmem device at 00:10.0 whose
 * shared page xtask keeps filled with the host battery's state (or fixed
 * values). These methods read that page, so WaveOS sees the real battery
 * through the same _BIF/_BST/_PSR methods a laptop's firmware provides.
 *
 * Page layout (little-endian 32-bit words):
 *   0 magic "WVBT"   1 state (1 discharging, 2 charging, 0 idle)
 *   2 rate (mW)      3 remaining (mWh)    4 voltage (mV)
 *   5 design (mWh)   6 last full (mWh)    7 adapter online (0/1)
 *
 * Rebuild with: iasl -p host-battery assets/acpi/host-battery.asl
 */
DefinitionBlock ("", "SSDT", 2, "WAVEOS", "HOSTBATT", 1)
{
    External (\_SB.PCI0, DeviceObj)

    Scope (\_SB.PCI0)
    {
        Device (BSHM)
        {
            Name (_ADR, 0x00100000)
            OperationRegion (CFG, PCI_Config, 0x18, 8)
            Field (CFG, DWordAcc, NoLock, Preserve)
            {
                B2LO, 32,
                B2HI, 32
            }

            /* Word `Arg0` of the shared page, or Ones when it isn't there. */
            Method (WORD, 1, Serialized)
            {
                Local0 = (B2LO & 0xFFFFFFF0) | (B2HI << 32)
                If (Local0 == 0)
                {
                    Return (Ones)
                }
                OperationRegion (SHM, SystemMemory, Local0, 32)
                Field (SHM, DWordAcc, NoLock, Preserve)
                {
                    W0, 32, W1, 32, W2, 32, W3, 32,
                    W4, 32, W5, 32, W6, 32, W7, 32
                }
                If (W0 != 0x54425657)
                {
                    Return (Ones)
                }
                Switch (ToInteger (Arg0))
                {
                    Case (1) { Return (W1) }
                    Case (2) { Return (W2) }
                    Case (3) { Return (W3) }
                    Case (4) { Return (W4) }
                    Case (5) { Return (W5) }
                    Case (6) { Return (W6) }
                    Case (7) { Return (W7) }
                }
                Return (Ones)
            }
        }
    }

    Scope (\_SB)
    {
        Device (BAT0)
        {
            Name (_HID, EisaId ("PNP0C0A"))
            Name (_UID, 1)
            Method (_STA, 0, NotSerialized)
            {
                If (\_SB.PCI0.BSHM.WORD (5) == Ones)
                {
                    Return (0x0F)
                }
                Return (0x1F)
            }
            Method (_BIF, 0, NotSerialized)
            {
                Local0 = Package (13)
                {
                    0, 0, 0, 1, 0, 0, 0, 100, 100,
                    "Host battery", "0001", "LION", "WaveOS"
                }
                Local0[1] = \_SB.PCI0.BSHM.WORD (5)
                Local0[2] = \_SB.PCI0.BSHM.WORD (6)
                Local0[4] = \_SB.PCI0.BSHM.WORD (4)
                Local0[5] = \_SB.PCI0.BSHM.WORD (6) / 10
                Local0[6] = \_SB.PCI0.BSHM.WORD (6) / 25
                Return (Local0)
            }
            Method (_BST, 0, NotSerialized)
            {
                Local0 = Package (4) { 0, 0, 0, 0 }
                Local0[0] = \_SB.PCI0.BSHM.WORD (1)
                Local0[1] = \_SB.PCI0.BSHM.WORD (2)
                Local0[2] = \_SB.PCI0.BSHM.WORD (3)
                Local0[3] = \_SB.PCI0.BSHM.WORD (4)
                Return (Local0)
            }
        }

        Device (ADP0)
        {
            Name (_HID, "ACPI0003")
            Method (_STA, 0, NotSerialized) { Return (0x0F) }
            Method (_PSR, 0, NotSerialized)
            {
                Return (\_SB.PCI0.BSHM.WORD (7) == 1)
            }
        }

        Device (LID0)
        {
            Name (_HID, EisaId ("PNP0C0D"))
            Method (_LID, 0, NotSerialized) { Return (1) }
        }
    }
}
