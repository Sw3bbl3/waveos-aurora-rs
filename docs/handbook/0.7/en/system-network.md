# Networking and audio
WaveOS supports wired/emulated virtio-net and selected Intel e1000/e1000e adapters, IPv4, DHCP, DNS, TCP/UDP, HTTP and TLS. Wi-Fi and IPv6 are not implemented.

## Check a connection
1. Start QEMU with its default network or `--net e1000e`.
2. Open Settings → Network or run `ifconfig`.
3. Try `nslookup example.com`, `ping example.com`, and `fetch -i https://example.com`.
4. Open a simple page in Nebula.

A failed ping alone does not prove HTTP is unavailable. Check DHCP and DNS separately. `--net none` intentionally disables the guest network; Learn still works.

## Check sound
1. Open Settings → Sound or click the speaker in the menu bar.
2. Unmute and set a comfortable volume.
3. Run `play --tone 440 500` for a short test.

Audio uses Intel HD Audio. Headless runs have no host sound output; a silent host does not establish a guest driver failure. Use `volume` and `lspci` to inspect configuration.
