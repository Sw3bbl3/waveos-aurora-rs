# Matériel et alimentation
L’architecture prise en charge est x86-64 avec UEFI. Les pilotes comprennent AHCI, virtio-blk moderne, NVMe, les claviers/souris/tablettes/supports USB xHCI, Intel HD Audio et certains adaptateurs réseau. La compatibilité dépend du périphérique; elle n’est pas garantie pour tous les PC.

## Examiner le matériel
1. Exécutez `cpuinfo`, `lspci` et `lsusb` dans Terminal.
2. Exécutez `battery` sur un portable ou utilisez la configuration de test QEMU `--battery`.
3. Conservez la sortie de `dmesg` pour diagnostiquer un périphérique.
4. Consultez la validation de la version pour connaître les configurations réellement testées.

## Veille et arrêt
Enregistrez votre travail avant de choisir Sleep. S3 dépend du micrologiciel et de la reprise des pilotes. Modern Standby, les pavés tactiles I²C et un pilote graphique natif ne sont pas disponibles. Un écran peut ne pas reprendre sur du matériel incompatible.

## Dépannage
Utilisez un clavier et une souris USB si le pavé tactile intégré n’est pas pris en charge. Si la veille échoue, redémarrez depuis une machine virtuelle arrêtée ou le menu du micrologiciel et examinez les journaux. Les résultats QEMU ne constituent pas une certification du matériel physique.
