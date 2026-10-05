# Format des paquets GINA
Un paquet .gina est une archive ustar non compressée et restreinte, avec manifest.toml à la racine. Le validateur vérifie les chemins, les sommes de contrôle, les limites, les ressources, l’architecture x86-64, le SDK et l’ABI 1. Les limites sont de 64 Mio et 1 024 entrées.

## Emballer une application compilée
1. Compilez un véritable exécutable userland WaveOS sur l’hôte.
2. Créez un manifeste avec des chaînes simples, des entiers et des tableaux de chaînes :
```toml
format = 1
id = "dev.example.myapp"
name = "My App"
developer = "Example Developer"
version = "1.0.0"
architecture = "x86_64"
sdk = "0.7"
abi = 1
entry = "bin/app"
icon = ""
resources = []
extensions = []
```
3. Exécutez `cargo xtask package manifest.toml app.elf MyApp.gina`.
4. Transférez le paquet dans WaveOS et ouvrez-le dans GINA Apps.

## Commandes natives
Utilisez `constellation inspect PACKAGE`, `constellation install PACKAGE`, `constellation list` et `constellation launch APP_ID`. `constellation new PROJECT APP_ID NAME` crée les sources d’un projet; cette commande n’installe pas de compilateur.

## Stockage et mises à jour
Les fichiers sont préparés sous /Applications/APP_ID. Un enregistrement actif sélectionne une génération validée après synchronisation des écritures. /AppData/APP_ID demeure après une mise à jour ou une désinstallation normale. Une mise à jour invalide conserve l’application active précédente. Les signatures, les permissions et l’isolation des données entre applications restent à venir.
