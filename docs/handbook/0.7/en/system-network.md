# Networking and audio
WaveOS supports wired/emulated virtio-net and selected Intel e1000/e1000e adapters, IPv4, DHCP, DNS, TCP/UDP, HTTP and TLS. IPv6 includes neighbor discovery, SLAAC, ICMPv6, TCP and UDP. Wi-Fi includes a native supplicant and simulated networks for development; recognized Intel radios are not yet operational on physical hardware.

## Check a connection
1. Start QEMU with its default network or `--net e1000e`.
2. Open Settings → Network or run `ifconfig`.
3. Choose **Check connection** in Settings, or run `netcheck https://example.com`. This checks addresses, DNS, TCP, and verified HTTPS on demand. Having an address means the network is configured; it does not prove Internet access.
4. Open a page in Voyage.

A failed ping alone does not prove HTTP is unavailable. Check DHCP and DNS separately. `--net none` intentionally disables the guest network; Learn still works.

`nslookup example.com`, `ping example.com`, and `fetch -i https://example.com` provide individual checks. Native diagnostics and browser compatibility are separate: a successful native request does not prove that a website's JavaScript or sign-in works.

Voyage keeps cookies and site data across restarts. Failed navigation provides an explanation, Back, and Retry; Escape stops loading. Downloads show progress, Cancel, and the saved location, and choose a new name instead of overwriting an existing file. Keep the date and UTC offset correct in Settings: TLS certificates and cookies depend on UTC time. Certificate failures are never bypassed automatically.

## Check sound

Firefox uses software decoding in its content process because the remote decoder
configuration currently stalls on the Linux compatibility layer. Codecs and
quality choices remain available; decoder crash isolation is reduced. QEMU CPU
emulation can still limit video smoothness. This does not establish hardware
acceleration or audible output on a physical computer.

1. Open Settings → Sound or click the speaker in the menu bar.
2. Unmute and set a comfortable volume.
3. Run `play --tone 440 500` for a short test.

Audio uses Intel HD Audio. Headless runs have no host sound output unless `--audio-wav PATH` records the guest output; a silent host does not establish a guest driver failure. Use `volume` and `lspci` to inspect configuration.
