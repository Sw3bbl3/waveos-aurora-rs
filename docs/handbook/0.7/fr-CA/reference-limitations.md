# Limites connues
WaveOS 0.7 est un système d’exploitation en développement. Un numéro de version ne certifie ni une aptitude à la production ni une compatibilité universelle.

## Limites actuelles
- Aucune chaîne Rust/Cargo/LLVM native, aucun service sémantique d’EDI dans Studio et aucun débogueur source.
- La compatibilité JavaScript et Web est partielle. Les modules JavaScript, BigInt, Intl, SVG et WebP sont absents; les sites complexes et les services de diffusion protégée peuvent échouer.
- Aucun Wi-Fi, pavé tactile I²C, pilote graphique natif ou Modern Standby. IPv6 offre la découverte de voisins, SLAAC, ICMPv6, TCP et UDP; sa validation QEMU ne certifie pas les réseaux physiques.
- La lecture native MP4/H.264 et AAC-LC est expérimentale; la gestion des DRM et la compatibilité générale YouTube/TikTok ne sont pas établies.
- GINA valide la structure, mais ne fournit ni signature ni isolation des permissions des données.
- Le bureau et les autres applications conservent leurs interfaces anglaises; Learn et ses guides offrent l’anglais et le français canadien.
- Les sessions ISO exigent un volume compatible accessible en écriture pour conserver les fichiers personnels.

## Vérifier avant de compter sur une fonction
1. Trouvez son guide et consultez les prérequis.
2. Cherchez dans le rapport de validation un test réellement effectué avec la configuration pertinente.
3. Conservez des sauvegardes et faites un petit essai réversible avant d’utiliser des données importantes.

Les performances QEMU décrivent la configuration d’émulation testée. Elles ne démontrent ni la fréquence d’images sur matériel natif, ni l’efficacité énergétique de l’hôte, ni la capture physique du clavier.
