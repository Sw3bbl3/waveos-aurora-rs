# Installation et mise à jour
Utilisez une machine virtuelle UEFI x86-64. Sur un Mac Apple Silicon, QEMU émule le système invité x86-64; WaveOS ne démarre pas directement sur Apple Silicon. Le BIOS traditionnel n’est pas pris en charge.

## Avant de commencer
Téléchargez l’image de la version 0.7.0 et SHA256SUMS. Vérifiez le téléchargement avec `shasum -a 256 FICHIER` sous macOS ou `sha256sum FICHIER` sous Linux. Conservez une copie de tout disque WaveOS existant avant la mise à jour.

## Exécuter à partir du code source
1. Installez Rust avec rustup et QEMU avec le micrologiciel OVMF correspondant. Sous macOS, utilisez `brew install qemu`; sous Debian/Ubuntu, `sudo apt install qemu-system-x86 ovmf`.
2. Récupérez le code source de la version. rustup installe la chaîne Rust datée.
3. Exécutez `cargo xtask run`. La machine virtuelle utilise par défaut quatre processeurs et 512 Mio de mémoire.
4. Enregistrez un fichier dans Documents, redémarrez WaveOS à partir de son menu et vérifiez que le fichier est toujours présent.

Le lanceur met à jour la partition système en conservant le volume personnel. L’option `--fresh-disk` efface le disque sélectionné; réservez-la aux images de test jetables.

## ISO et USB
1. Pour une machine virtuelle démarrant sur un disque optique, montez l’ISO et activez UEFI. Prévoyez au moins 2 Gio de mémoire pour la configuration VirtualBox décrite.
2. La source ISO est en lecture seule. Les fichiers personnels sont temporaires sans volume AuroraFS compatible et accessible en écriture.
3. Pour démarrer sur USB, écrivez l’IMG décompressé sur le périphérique USB entier avec un outil d’imagerie. Cette opération efface le périphérique. Vérifiez d’abord son identité.
4. Sélectionnez le périphérique USB dans le menu de démarrage. Consultez [Matériel](system-hardware.md).

## Mettre à jour une copie existante
Sauvegardez l’image persistante, mettez le code source à jour, puis exécutez `cargo xtask run`. N’écrasez jamais votre disque personnel avec l’IMG vierge de la version pour effectuer une mise à jour. La version 0.7 n’offre pas de mise à jour automatique dans le système.

## Dépannage
Si aucun support amorçable n’est détecté, vérifiez UEFI et l’image sélectionnée. Si la disposition du disque est incompatible, conservez-le et restaurez la sauvegarde; n’utilisez pas `--fresh-disk` pour le réparer. Les plateformes réellement testées sont indiquées dans les résultats de validation.
