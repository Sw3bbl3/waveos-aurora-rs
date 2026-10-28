# WaveSDK
Le WaveSDK contient tout ce qu’il faut pour créer des applications WaveOS : le cadriciel WaveUI, CoreKit pour les fichiers, les processus et le réseau, Studio, la commande `wavesdk` et le format de paquet GINA. Le SDK s’appelait auparavant Constellation. L’ancienne commande `constellation` fonctionne toujours, et les paquets qui indiquent les SDK 0.6 à 0.8 s’installent encore.

## Contenu du SDK
| Élément | Rôle |
|---|---|
| WaveUI | Interfaces déclaratives dans le style de WaveOS. Consultez [WaveUI](developer-waveui.md). |
| CoreKit | Fichiers, processus, fils d’exécution, préférences, presse-papiers, notifications, réseau et communications interprocessus |
| Studio | Créer des projets, modifier le code, concevoir une fenêtre, exporter ou installer des paquets |
| `wavesdk` | Les mêmes opérations depuis Terminal |
| GINA | Le format des paquets d’applications. Consultez [Format des paquets GINA](developer-packages.md). |

Les sources du SDK se trouvent dans `/System/Developer/SDK`. L’exemple WaveUI Gallery, qui présente toutes les commandes, se trouve dans `/System/Developer/Examples/WaveUIGallery.gina`.

## Créer un projet
1. Ouvrez Studio et choisissez **New**, ou exécutez `wavesdk new /Documents/Projects/MyApp dev.example.myapp "Mon app"`.
2. Modifiez l’application dans `src/main.rs`. Elle commence par une petite application WaveUI avec un bouton.
3. Concevez des écrans simples dans Design. Studio les enregistre dans `layouts/main.ui` et écrit le code WaveUI dans `src/generated_ui.rs`. Votre `src/main.rs` n’est jamais écrasé.
4. Enregistrez le projet.

Un projet contient :
```text
MyApp/
  manifest.toml        identité, version et SDK de l’application
  Cargo.toml           dépend de corekit et de waveui dans le SDK
  src/main.rs          votre code
  src/generated_ui.rs  écrit à partir de layouts/main.ui
  layouts/main.ui      un composant par ligne : heading, label, button, toggle
```

## Commandes
| Commande | Résultat |
|---|---|
| `wavesdk doctor` | Liste les outils natifs manquants |
| `wavesdk new PROJET ID_APP NOM` | Crée un projet |
| `wavesdk generate PROJET` | Écrit `src/generated_ui.rs` à partir de la mise en page |
| `wavesdk build PROJET` et `wavesdk run PROJET` | Compilent et exécutent, lorsque les outils natifs sont présents |
| `wavesdk package PROJET` | Exporte un `.gina` à partir de `build/app.elf` |
| `wavesdk inspect PAQUET` | Affiche le nom, l’identifiant, le développeur, la version et le nombre de fichiers |
| `wavesdk install PAQUET` | Installe ou met à jour une application |
| `wavesdk list` | Liste les applications installées |
| `wavesdk launch ID_APP` | Lance une application installée |
| `wavesdk uninstall ID_APP` | Retire une application en conservant ses données |

## Compiler sur un hôte
Rust, Cargo et LLVM ne fonctionnent pas encore dans WaveOS. Build et Run signalent les outils manquants; ils ne simulent jamais une réussite. En attendant le portage, compilez les applications sur un hôte avec le dépôt :

1. Ajoutez votre application à l’espace de travail userland, avec `waveui` et `corekit` comme dépendances.
2. Exécutez `cargo xtask build` pour compiler le système et votre application.
3. Emballez l’exécutable avec `cargo xtask package manifest.toml app.elf MyApp.gina`.
4. Ouvrez le paquet dans Files, ou exécutez `wavesdk install MyApp.gina`.

## Tester votre application
Avant de la distribuer, vérifiez que votre application :
- Se redimensionne proprement, jusqu’à sa taille minimale.
- Fonctionne au clavier seulement, avec Tab, Espace, Retour et Échap.
- S’affiche correctement en apparence claire et sombre, et avec une autre couleur d’accent.
- Respecte Réduire les animations et le texte agrandi.
- Demande confirmation avant de fermer une fenêtre dont le travail n’est pas enregistré.
