# Files
Gérez les documents et les volumes montés.

## Utiliser l’application
1. Ouvrez Files et choisissez Documents, Pictures ou un emplacement dans la barre latérale.
2. Utilisez New Folder, F2 pour renommer, puis Ctrl+C/Ctrl+X et Ctrl+V pour copier ou déplacer.
3. Ctrl+Maj+1/Ctrl+Maj+2 choisit la vue par icônes ou par liste; Ctrl+F lance la recherche. Espace ouvre Quick Look.
4. Suppr. déplace un élément dans Trash. Ouvrez Trash et utilisez Put Back pour le restaurer.

## Résultat attendu et dépannage
Faites glisser les éléments entre les fenêtres ou vers un dossier; maintenez Ctrl pour copier. Empty Trash supprime définitivement les éléments. Si un volume USB manque, consultez `lsusb` et vérifiez le système de fichiers. /System est en lecture seule.

Voir [Bureau et fenêtres](desktop-windows.md) pour les raccourcis communs et [Récupération](reference-recovery.md) si l’application ne s’ouvre pas.

## Onglets et transferts
Ctrl+T ouvre un onglet; Ctrl+Tab (ou Ctrl+Maj+Tab) change d’onglet; Ctrl+1–8 sélectionne un onglet; Ctrl+W le ferme. Les huit onglets possibles conservent chacun leur emplacement, sélection, recherche, historique et défilement pendant la session.

Les copies, déplacements et duplications s’exécutent en arrière-plan. Le panneau de transfert affiche l’élément courant, les octets copiés, les éléments terminés et les erreurs. Un seul transfert s’exécute par fenêtre; la navigation reste disponible. Ctrl+J place le focus sur les commandes du transfert.

En cas de conflit, choisissez Replace, Keep Both ou Skip. Replace s’applique aux fichiers ordinaires distincts; les dossiers et la copie d’un élément sur lui-même proposent Keep Both ou Skip. Apply all applique le choix aux conflits compatibles du transfert en cours. Lorsque le panneau a le focus, utilisez Tab/Entrée ou R/K/S/A; Échap annule.

L’annulation conserve les éléments terminés et retire les copies temporaires de cette opération. Fermer la fenêtre propose Keep Open ou Cancel & Close et attend le nettoyage. Next error parcourt les erreurs du résultat. Une interruption de courant peut laisser des dossiers cachés `.waveos-transfer-*`; ne les retirez pas pendant un transfert. Files ne garantit pas un remplacement transactionnel sur tous les systèmes de fichiers montés.
