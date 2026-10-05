# Stockage et persistance
AuroraFS est le système de fichiers journalisé du volume personnel. Sa signature sur disque demeure compatible avec les anciennes images WaveOS. /System contient les applications en lecture seule; /Boot expose la partition FAT de démarrage; les volumes amovibles se trouvent sous /Volumes.

## Vérifier la persistance
1. Dans Terminal, exécutez `echo persistence > /Documents/check.txt`.
2. Exécutez `sync`, puis redémarrez à partir du menu Aurora.
3. Ouvrez `/Documents/check.txt` dans Notes ou utilisez `cat`.

Le texte devrait être conservé sur un disque AuroraFS accessible en écriture. Un ISO sans volume personnel accessible en écriture utilise du stockage temporaire.

## Bonnes pratiques
Éjectez les supports amovibles dans Files avant de les débrancher. Arrêtez le système à partir de son menu pour vider les caches d’écriture. Sauvegardez l’image disque sur l’hôte lorsque la machine virtuelle est arrêtée; une copie d’une image en cours de modification ne devrait jamais être votre seule sauvegarde.

## Dépannage
Utilisez `df` pour vérifier l’espace. Un volume plein peut empêcher l’enregistrement de documents, de réglages ou de paquets. Ne formatez pas une partition inconnue pour corriger un échec de montage. Restaurez une copie fonctionnelle et conservez l’original pour l’examiner.
