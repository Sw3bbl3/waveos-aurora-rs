# Matériel et alimentation
L’architecture prise en charge est x86-64 avec UEFI. Les pilotes comprennent AHCI, virtio-blk moderne, NVMe, les claviers/souris/tablettes/supports USB xHCI, Intel HD Audio et certains adaptateurs réseau. La compatibilité dépend du périphérique; elle n’est pas garantie pour tous les PC.

## Ce qu’il faut à un PC
- Un processeur Intel ou AMD 64 bits et un micrologiciel UEFI avec le démarrage sécurisé (Secure Boot) désactivé; l’ancien BIOS ne peut pas démarrer WaveOS.
- 512 Mio de mémoire pour le bureau et les applications de WaveOS, **2 Gio pour Voyage ou Firefox**, de 4 à 8 Gio recommandés. Il n’y a pas d’échange sur disque : un programme qui manque de mémoire échoue.
- Une clé USB ou un disque de 2 Gio avec les paquets Linux (512 Mio sans eux); 8 Gio ou plus sont recommandés.
- Pour le réseau, un adaptateur filaire Intel e1000 ou e1000e.

Le tableau complet se trouve dans `docs/HARDWARE.md`, dans les sources de WaveOS.

## Examiner le matériel
1. Exécutez `cpuinfo`, `lspci` et `lsusb` dans Terminal.
2. Exécutez `battery` sur un portable ou utilisez la configuration de test QEMU `--battery`.
3. Conservez la sortie de `dmesg` pour diagnostiquer un périphérique.
4. Consultez la validation de la version pour connaître les configurations réellement testées.

## Veille et arrêt
Enregistrez votre travail avant de choisir Sleep. S3 dépend du micrologiciel et de la reprise des pilotes. Modern Standby, les pavés tactiles I²C et un pilote graphique natif ne sont pas disponibles. Un écran peut ne pas reprendre sur du matériel incompatible.

## Dépannage
Utilisez un clavier et une souris USB si le pavé tactile intégré n’est pas pris en charge. Si la veille échoue, redémarrez depuis une machine virtuelle arrêtée ou le menu du micrologiciel et examinez les journaux. Les résultats QEMU ne constituent pas une certification du matériel physique.
