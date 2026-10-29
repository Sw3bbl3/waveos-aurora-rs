# Réseau et audio
WaveOS prend en charge virtio-net et certains adaptateurs Intel e1000/e1000e, IPv4, DHCP, DNS, TCP/UDP, HTTP et TLS. IPv6 comprend la découverte de voisins, SLAAC, ICMPv6, TCP et UDP. Le Wi-Fi n’est pas implémenté.

## Vérifier une connexion
1. Démarrez QEMU avec le réseau par défaut ou `--net e1000e`.
2. Ouvrez Settings → Network ou exécutez `ifconfig`.
3. Essayez `nslookup example.com`, `ping example.com` et `fetch -i https://example.com`.
4. Ouvrez une page dans Firefox.

L’échec d’un ping ne prouve pas à lui seul que HTTP est indisponible. Vérifiez DHCP et DNS séparément. `--net none` désactive volontairement le réseau invité; Learn continue de fonctionner.

## Vérifier le son
1. Ouvrez Settings → Sound ou cliquez sur le haut-parleur dans la barre de menus.
2. Réactivez le son et réglez un volume confortable.
3. Exécutez `play --tone 440 500` pour un court test.

L’audio utilise Intel HD Audio. Les exécutions sans affichage n’ont pas de sortie audio sur l’hôte, sauf si `--audio-wav PATH` enregistre la sortie invitée; le silence de l’hôte ne prouve donc pas une panne du pilote invité. Utilisez `volume` et `lspci` pour examiner la configuration.
