# Développement Pulsar et multimédia
Développez et testez JavaScript et les médias du navigateur sur l’hôte, puis vérifiez le comportement dans le vrai système invité. Prérequis : la chaîne Rust épinglée et la configuration de [Compilation et tests](developer-build.md).

## Exécuter JavaScript
1. Ouvrez Terminal dans WaveOS et lancez `js` pour une invite interactive, ou `js /Documents/example.js` pour un script enregistré.
2. Dans Nebula, ouvrez Pulsar playground hors ligne depuis la page de départ. Vérifiez les événements, les minuteries et le stockage avant une page distante.
3. Sur l’hôte, exécutez `cargo run --release -p nebula-script --example js -- FILE.js`. Ajoutez `--dump` pour inspecter le bytecode.

Pulsar compile en bytecode et l’interprète avec un ramasse-miettes. Les fonctions ordinaires des gros scripts peuvent être compilées au premier appel. Les caches de propriétés, les identifiants internés et le cache de scripts compilés partagé entre onglets réduisent le travail répété; ils ne fournissent pas une compatibilité Web universelle. Le code hôte doit protéger les valeurs JavaScript conservées entre les rappels et microtâches avec `Realm::with_roots`.

## Valider le navigateur et les médias
1. Exécutez `cargo test -p nebula-script -p nebula-engine -p nebula-media` sur l’hôte.
2. Utilisez le MP4 CC0 et le signal AAC généré dans `libs/media/tests/fixtures`; leur provenance y est documentée. Testez aussi les entrées invalides et tronquées.
3. Compilez avec `cargo xtask build` et démarrez un disque d’essai distinct. Utilisez `--audio-wav PATH` sans affichage lorsqu’un enregistrement audio est nécessaire.
4. Vérifiez lecture/pause, changement de position, sourdine, changement d’onglet et fin du flux. Examinez les erreurs série et comparez la sortie au fichier connu.

Résultat attendu : les tests hôtes réussissent et les petits fichiers pris en charge sont décodés dans l’invité. Les tests des codecs sur l’hôte ne prouvent pas le fonctionnement d’un site de diffusion distant. Reprenez les tests de conformité après une modification du moteur; les instantanés historiques test262 ne décrivent pas chaque changement plus récent.

## Dépannage et limites
Séparez les mesures d’exécution JavaScript, d’analyse/compilation, de mise en page, de réseau et de décodage. Ne déduisez pas un gain de réactivité du navigateur d’un test de compilation sur l’hôte. Les modules JavaScript, BigInt, Intl, DRM et la compatibilité générale avec les sites de diffusion restent incomplets. Le compilateur et le débogueur Rust natifs sont toujours absents; l’exécution JavaScript ne fournit pas cette chaîne d’outils.

Voir [Nebula](apps-nebula.md) pour les raccourcis et [Validation](reference-validation.md) pour les exigences de mesure.
