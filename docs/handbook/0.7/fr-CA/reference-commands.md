# Référence des commandes Terminal
Les programmes se trouvent dans /System/Bin. Exécutez `help` dans Terminal pour les commandes de l’interpréteur; les outils affichent leur utilisation lorsque des arguments sont manquants ou invalides.

| Tâche | Commandes |
|---|---|
| Fichiers | ls, cat, cp, mv, rm, mkdir, touch |
| Texte | echo, grep, wc |
| Processus | ps, kill |
| Stockage | df, sync |
| Système | uname, uptime, date, mem, neofetch, cpuinfo, dmesg |
| Périphériques | lspci, lsusb, battery |
| Réseau | ifconfig, nslookup, ping, fetch |
| Son | play, volume |
| Applications | constellation, gina-check, gina-demo |

## S’exercer prudemment
1. Créez un dossier temporaire avec `mkdir /Documents/practice`.
2. Écrivez `echo hello > /Documents/practice/message.txt`.
3. Comptez son contenu avec `cat /Documents/practice/message.txt | wc`.
4. Copiez-le avec `cp /Documents/practice/message.txt /Documents/practice/copy.txt`.

Résultat attendu : deux fichiers lisibles. La redirection > remplace un fichier; >> ajoute à la fin. `rm` supprime directement sans passer par Trash. Vérifiez les chemins avant les commandes destructives.

## Dépannage
Les noms de commandes et les chemins distinguent les majuscules des minuscules. Les programmes peuvent n’implémenter qu’une partie des options Unix habituelles. Ne supposez pas que les scripts shell, les gestionnaires de paquets ou les outils Rust natifs sont disponibles.

## Commandes de l’interpréteur
Utilisez `cd DIR` pour changer de dossier, `history` pour revoir les commandes, `clear` ou Ctrl+L pour effacer l’affichage et `exit` pour fermer Terminal. `theme light` et `theme dark` choisissent l’apparence. `open Learn` lance une application; `open /Documents/practice/message.txt` ouvre un fichier. `shutdown` et `reboot` agissent sur tout le système.

## Autres exemples
```sh
ls /Documents | grep txt
ps
uname
ifconfig
nslookup example.com
fetch https://example.com
sync
```
Les exemples réseau nécessitent un adaptateur configuré et un réseau accessible. `a && b` exécute b après une réussite; `a || b` exécute b après un échec; `a; b` exécute les deux. Ctrl+C arrête le programme au premier plan. Mettez les chemins contenant des espaces entre guillemets. Utilisez `kill PID` seulement après avoir identifié le processus voulu avec `ps`.
