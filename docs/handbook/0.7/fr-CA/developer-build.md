# Compilation et tests
Compilez sous macOS ou Linux avec Rust/rustup, QEMU et OVMF. La création d’un ISO exige aussi xorriso (`brew install xorriso` ou `sudo apt install xorriso`).

## Compiler le système
1. Utilisez la chaîne datée dans rust-toolchain.toml. `tools/toolchain-baseline.toml` indique la révision Rust et la version LLVM attendues; xtask refuse une incompatibilité.
2. Exécutez `cargo xtask docs` pour valider et compiler les deux langues du guide.
3. Exécutez `cargo xtask build` pour compiler le chargeur, le noyau et les applications.
4. Exécutez `cargo xtask run` pour démarrer le bureau persistant.
5. Exécutez `cargo xtask image` pour une image USB/disque vierge ou `cargo xtask iso` pour une image optique.

## Exécuter les vérifications
```sh
cargo test -p aurora-help -p gina -p aurorafs -p fat32 -p lumen -p aurora-image -p aurora-wav -p nebula-web -p nebula-secure -p nebula-engine -p xtask
cargo xtask test --disk ahci --smp 4 --net virtio
cargo xtask test --disk virtio --smp 4 --net virtio
cargo xtask test --disk nvme --smp 4 --net e1000e
```
Le banc d’essai du noyau démarre deux fois le disque de test pour vérifier la persistance. Testez aussi la configuration à un processeur. Exécutez `cargo fmt --all -- --check` à la racine et dans userland avant de contribuer.

## Dépannage
L’espace userland utilise une cible et un script d’édition de liens personnalisés; compilez avec xtask. Si le compilateur ne correspond pas, installez la chaîne épinglée plutôt que de modifier silencieusement la référence. Consultez la sortie série avant de considérer un bureau visible comme une validation complète.
