# WaveOS Aurora 0.7.0 — validation / validation

October 5, 2026 / 5 octobre 2026

The downloadable validation archive records the final source commit, toolchain, CI run, artifact hashes, raw measurements, serial logs, and screenshots. Its `provenance.json` identifies the release build; `SHA256SUMS` verifies the public downloads. Only seeded sample data is included in release images.

L’archive de validation indique le commit final, la chaîne d’outils, l’exécution CI, les empreintes des fichiers, les mesures brutes, les journaux série et les captures. Son fichier `provenance.json` identifie la compilation; `SHA256SUMS` vérifie les téléchargements publics. Les images contiennent seulement des données d’exemple.

## Automated checks / Vérifications automatisées

- 98 host tests: help catalog/search, GINA, AuroraFS, FAT32, Lumen rendering/focus/window placement, image/audio codecs, HTTP/TLS, Nebula layout, and documentation generation.
- 33 kernel checks per boot, two boots per configuration: AHCI/1 CPU/virtio-net; AHCI/4 CPU/virtio-net; virtio-blk/4 CPU/virtio-net; NVMe/4 CPU/virtio-net; NVMe/4 CPU/e1000e.
- Persistence is checked on the second boot. Network tests include DHCP, ICMP, UDP, loopback TCP, a 1 MiB transfer, host transfer, and inbound connections. TCP regression coverage includes delayed cumulative ACKs after retransmission, sequence wraparound, and FIN acknowledgement.
- Process teardown retains the existing frame-reclamation invariant, with bounded waiting for asynchronous cleanup rather than a fixed sleep.
- Learn renders all 58 articles across both languages and exercises its missing-article error path. The same check passed with QEMU networking disabled. The build rejects missing translations, invalid links/anchors/images, raw HTML, and unsupported task lists, footnotes, and strikethrough.
- Root and userland formatting checks and complete normal/test builds are required. GitHub runner-acquisition cancellations are retried; they are not recorded as test passes.

Les 98 tests hôtes couvrent les bibliothèques et la documentation. Chaque configuration ci-dessus exécute 33 tests du noyau par démarrage, sur deux démarrages du même disque. La persistance, les transferts réseau, les limites des accusés de réception TCP et la récupération des ressources des processus sont vérifiés. Learn charge les 58 articles dans les deux langues et traite un article absent. La compilation vérifie les traductions, liens, ancres, images et éléments Markdown pris en charge. Les annulations causées par l’absence d’un exécuteur GitHub sont reprises, sans être considérées comme des réussites.

## Observed desktop checks / Vérifications du bureau

- 1280×800 and 2560×1440; English and Canadian French Learn content; light/dark appearance; Reduce Motion, Reduce Transparency, and Increase Contrast.
- Keyboard search, accent-insensitive French queries, category/article selection, back/forward, contents, PageUp/PageDown, reading size, and language retention. Images, wrapped code, and tables were inspected. A first-launch dark-theme colour bug found during reboot testing was corrected and rechecked.
- Left/right snapping, maximizing, restoring, minimum-size rejection, restoring floating size beneath a dragged title bar, and resolution changes.
- Alt+Tab selection, minimized status/restoration, cancellation, and blocking typing from the underlying Notes window. Control Center Enter leaves slider values unchanged; arrows adjust them. Settings search and accessibility controls were exercised with the keyboard.
- Bundled app opening/closing smoke checks, including Nebula, GINA Apps, and Constellation Studio. GINA’s eight lifecycle checks passed; the packaged gallery was installed, launched from `/Applications`, and remained registered after reboot.
- A saved document, Learn language/reading size, and appearance/accessibility preferences survived reboot. The original personal disk was separately verified to match its preserved backup.

Les vérifications visuelles couvrent les deux résolutions, les deux langues de Learn, les thèmes clair et sombre et les options d’accessibilité. Elles comprennent la recherche, la navigation, les images, le code, les tableaux, les états des fenêtres, le sélecteur et les commandes au clavier. Les applications intégrées ont été ouvertes et fermées. Les huit vérifications du cycle GINA ont réussi; la galerie empaquetée a été installée et lancée. Le document d’essai et les préférences ont survécu au redémarrage. Le disque personnel d’origine correspond toujours à sa copie de sauvegarde.

## Performance / Performance

Read the complete [English measurement report](../../handbook/0.7/en/reference-validation.md) or [rapport français](../../handbook/0.7/fr-CA/reference-validation.md). Raw samples are in [evidence](evidence/). The desktop comparison is against development commit `1011e1a`, not a preceding public release. It supports localized rendering improvements while also recording dragging/switching p95 regressions. Learn measurements are an initial absolute baseline.

La comparaison porte sur la base de développement `1011e1a`, et non sur une version publique précédente. Elle documente les gains du rendu localisé ainsi que les régressions du p95 pendant le déplacement et le changement de fenêtre. Les mesures Learn forment une première base absolue.

## Artifact validation / Validation des fichiers publiés

The release process boots the actual IMG and UEFI ISO, checks version and offline Learn content, tests writable-disk persistence and temporary ISO-only storage, and re-downloads published assets to compare SHA-256 hashes. The final archive includes these boot logs and the exact source/CI identity. The IMG uses fresh sample data, never the personal persistent disk.

Le processus démarre les vrais fichiers IMG et ISO UEFI, vérifie la version et Learn hors ligne, teste la persistance du disque et le stockage temporaire d’une session ISO seule, puis retélécharge les fichiers publiés pour vérifier leurs empreintes SHA-256. L’archive finale contient ces journaux et l’identité exacte du code et de la CI. L’IMG contient des données d’exemple neuves, jamais le disque personnel.

## Scope limits / Limites

QEMU validation does not certify physical PCs, VirtualBox, or physical Command-key capture on the host. Native compiler/debugger completion, virtual desktops, full translation of other app interfaces, Wi-Fi, IPv6, JavaScript, and native GPU drivers remain outside this release. App smoke checks verify basic opening/closing, not every application workflow. Timing measurements are not end-to-end responsiveness or memory claims.

La validation QEMU ne certifie ni les PC physiques, ni VirtualBox, ni la capture physique de la touche Command sur l’hôte. Le compilateur et le débogueur natifs, les bureaux virtuels, la traduction complète des autres interfaces, le Wi-Fi, IPv6, JavaScript et les pilotes GPU natifs restent hors de cette version. Les essais rapides des applications ne couvrent pas tous leurs parcours. Les mesures ne représentent ni la réactivité de bout en bout ni l’utilisation de la mémoire.
