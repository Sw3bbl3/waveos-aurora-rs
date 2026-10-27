/*
 * The kernel test run's laptop: a battery (73 %, discharging), a power
 * adapter (unplugged) and a lid (open), for QEMU, which has none.
 *
 * _BST is built the way much laptop firmware builds it, by storing into
 * package elements through Index, and the charge it reports depends on a
 * PCI configuration read at a non-zero offset (the Q35 host bridge's device
 * ID at 0x02). Both broke the AML interpreter before WaveOS's fixes (see
 * vendor/acpi/WAVEOS-PATCHES.md); the kernel test expects 73 %.
 *
 * Rebuild with: iasl -p test-battery assets/acpi/test-battery.asl
 */
DefinitionBlock ("", "SSDT", 2, "WAVEOS", "AURORBAT", 1)
{
    External (\_SB.PCI0, DeviceObj)

    Scope (\_SB.PCI0)
    {
        Device (HBCK)
        {
            Name (_ADR, 0)
            OperationRegion (CFG, PCI_Config, 0x02, 2)
            Field (CFG, WordAcc, NoLock, Preserve)
            {
                DID, 16
            }
        }
    }

    Scope (\_SB)
    {
        Device (BAT0)
        {
            Name (_HID, EisaId ("PNP0C0A"))
            Name (_UID, 1)
            Method (_STA, 0, NotSerialized) { Return (0x1F) }
            Method (_BIF, 0, NotSerialized)
            {
                Return (Package (13)
                {
                    0, 50000, 48000, 1, 11400, 2500, 1000, 100, 100,
                    "Aurora Test Cell", "0001", "LION", "WaveOS"
                })
            }
            Name (PBST, Package (4) { 0, 0, 0, 0 })
            Method (_BST, 0, Serialized)
            {
                PBST[0] = 1
                Store (9500, Index (PBST, 1))
                If (\_SB.PCI0.HBCK.DID == 0x29C0)
                {
                    PBST[2] = 35040
                }
                Else
                {
                    PBST[2] = 1
                }
                PBST[3] = 11100
                Return (PBST)
            }
        }

        Device (ADP0)
        {
            Name (_HID, "ACPI0003")
            Method (_STA, 0, NotSerialized) { Return (0x0F) }
            Method (_PSR, 0, NotSerialized) { Return (0) }
        }

        Device (LID0)
        {
            Name (_HID, EisaId ("PNP0C0D"))
            Method (_LID, 0, NotSerialized) { Return (1) }
        }
    }
}
