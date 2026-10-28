# Studio
Créez et modifiez des projets d’applications WaveUI et des interfaces simples avec le WaveSDK.

## Utiliser l’application
1. Ouvrez Studio et créez un projet dans `/Documents/Projects`.
2. Modifiez le Rust écrit à la main dans `src/main.rs`. Un nouveau projet commence par une petite application [WaveUI](developer-waveui.md).
3. Utilisez Design pour modifier `layouts/main.ui`; Studio écrit le code WaveUI correspondant dans `src/generated_ui.rs`.
4. Enregistrez le projet. L’exportation du paquet exige un véritable exécutable dans `build/app.elf`.

## Résultat attendu et dépannage
Rust/Cargo/LLVM natifs, les outils sémantiques et le débogueur source ne sont pas portés. Build & Run signale la chaîne manquante; il ne simule pas une réussite. La galerie incluse est compilée sur l’hôte.

Voir [Bureau et fenêtres](desktop-windows.md) pour les raccourcis communs et [Récupération](reference-recovery.md) si l’application ne s’ouvre pas.
