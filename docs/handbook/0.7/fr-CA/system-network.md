# Réseau et audio
WaveOS prend en charge virtio-net et certains adaptateurs Intel e1000/e1000e, IPv4, DHCP, DNS, TCP/UDP, HTTP et TLS. IPv6 comprend la découverte de voisins, SLAAC, ICMPv6, TCP et UDP. Le Wi-Fi comprend un client d’authentification natif et des réseaux simulés pour le développement; les radios Intel reconnues ne sont pas encore fonctionnelles sur le matériel physique.

## Vérifier une connexion
1. Démarrez QEMU avec le réseau par défaut ou `--net e1000e`.
2. Ouvrez Settings → Network ou exécutez `ifconfig`.
3. Choisissez **Check connection** dans Settings ou exécutez `netcheck https://example.com`. Cette vérification examine les adresses, DNS, TCP et HTTPS avec validation du certificat. Une adresse confirme la configuration réseau, sans prouver l’accès à Internet.
4. Ouvrez une page dans Voyage.

L’échec d’un ping ne prouve pas à lui seul que HTTP est indisponible. Vérifiez DHCP et DNS séparément. `--net none` désactive volontairement le réseau invité; Learn continue de fonctionner.

`nslookup example.com`, `ping example.com` et `fetch -i https://example.com` permettent des vérifications individuelles. Les diagnostics natifs et la compatibilité du navigateur sont distincts : une requête native réussie ne prouve pas que le JavaScript ou la connexion à un compte fonctionne sur un site.

Voyage conserve les témoins et les données des sites après un redémarrage. Une navigation échouée présente une explication et les actions Back et Retry; Échap arrête le chargement. Les téléchargements affichent la progression, l’action Cancel et l’emplacement du fichier enregistré. Un nouveau nom évite d’écraser un fichier existant. Vérifiez la date et le décalage UTC dans Settings : les certificats TLS et les témoins dépendent de l’heure UTC. Les erreurs de certificat ne sont jamais contournées automatiquement.

## Vérifier le son

Firefox utilise le décodage logiciel dans son processus de contenu, car la
configuration des décodeurs distants bloque actuellement dans la couche de
compatibilité Linux. Les codecs et les choix de qualité restent disponibles;
l’isolation des pannes de décodeur est réduite. L’émulation du processeur dans
QEMU peut encore limiter la fluidité vidéo. Cela ne confirme ni l’accélération
matérielle ni une sortie audio audible sur un ordinateur physique.

1. Ouvrez Settings → Sound ou cliquez sur le haut-parleur dans la barre de menus.
2. Réactivez le son et réglez un volume confortable.
3. Exécutez `play --tone 440 500` pour un court test.

L’audio utilise Intel HD Audio. Les exécutions sans affichage n’ont pas de sortie audio sur l’hôte, sauf si `--audio-wav PATH` enregistre la sortie invitée; le silence de l’hôte ne prouve donc pas une panne du pilote invité. Utilisez `volume` et `lspci` pour examiner la configuration.
