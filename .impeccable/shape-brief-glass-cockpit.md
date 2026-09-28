# Brief confirmé — refonte « Glass Cockpit »

Shape du 2026-09-28 · tirage `af696b2e` · direction choisie : assigned (Glass Cockpit) · build code-led
Référence visuelle : `.impeccable/mocks/decision/assigned.png` (source `directions.html#A`)
Produit : voir `PRODUCT.md`.

## 1. Pour qui
Devs Windows avec plusieurs sessions Claude Code / Codex en parallèle, qui jettent un œil à la barre des tâches entre deux prompts. Mode Operate : la lisibilité et la confiance priment sur l'expression.

## 2. Résultat et preuve
En moins d'une seconde : quel provider est en danger, quand il repart à zéro.
Géométrie du rythme : le remplissage montre le % consommé, le repère magenta la part de la fenêtre écoulée (bornée à 10 % comme `pace.rs`) ; rythme = remplissage ÷ repère.

## 3. Direction
Verre noir, Bahnschrift condensée (fournie avec Windows), valeurs encadrées, couleurs de rythme vert / ambre / rouge, magenta réservé au repère.
Règles venues des challengers refusés :
- **Un seul point chaud** : seule la fenêtre rouge au rythme le plus élevé est en inversé (fond rouge) ; les autres rouges ont des chiffres rouges, l'ambre a chiffres et cadre ambre, le vert est neutre. Aucune inversion s'il n'y a pas de rouge.
- **Une seule taille de texte dans le widget** : la hiérarchie passe par la graisse, le cadre et l'inversion.
- **États hors rythme = marques** : non reporté → jauge vide et `--` ; provider en échec → code barré, `--`, et la cause en mots dans le flyout. Jamais de rouge pour ces cas.
- **Une seule échelle** : axe 0–100 et graduations 25/50/75 identiques partout.
- **Le reset est le seul mouvement** : la jauge se vide d'un geste d'environ 250 ms, désactivé si Windows coupe les animations.

## 4. Périmètre
- **Widget** : environ 143 px pour Claude + Codex (342 aujourd'hui). Par ligne : code (CL / CX / AG), jauge 5h avec repère, fine ligne 7d dessous, % encadré, compte à rebours.
- **Flyout** (nouveau, au clic, **remplace l'infobulle du widget**) : liste d'alertes, une jauge verticale par fenêtre (5H, 7D, per-model), % encadré, heure exacte de reset (format régional Windows), compte à rebours détaillé, légende du repère sur une ligne, Refresh et Settings.
- **Icônes du tray** : la valeur 5h encadrée, avec les mêmes règles de couleur et d'inversion. L'infobulle native du tray (nom du provider) est conservée.
- **Inchangés** : le menu contextuel natif, les données et le polling, les clés de `settings.json`, le clic gauche sur le tray qui affiche ou masque le widget.
- **À éviter** : le déguisement aviation (bezels, vis, lueurs, dégradés, ombres portées dans le widget), le magenta ailleurs que sur le repère, les couleurs de marque des providers dans le widget (l'orange de Claude se confond avec l'ambre).

## 5. États et plages
- 1 provider : une ligne par fenêtre (codes 5H / 7D / FABLE). 2–3 providers : une ligne par provider (5h + ligne 7d). À 3 lignes, environ 12 px chacune : à vérifier en premier.
- La largeur suit le contenu (la largeur fixe n'est plus exigée).
- `pace_colors` désactivé : remplissage blanc instrument, pas d'inversion, le repère reste.
- `detailed_time` : compte à rebours `3h40`, le slot s'élargit.
- **Thème clair = mode jour** : plaque claire, encres foncées, seconde palette ; les trois bandes doivent garder un écart de luminosité (vérifier le daltonisme).
- DPI de 100 à 200 % via `sc()`. 11 langues ; les codes CL/CX/AG ne se traduisent pas, les libellés du flyout si.
- Tray à 100 % : « 100 » doit tenir dans 16 px en Bahnschrift condensée, sinon il faut une règle.

## 6. Interaction
- **Glisser n'importe où** : un clic ouvre le flyout ; un glissement au-delà du seuil Windows (`SM_CXDRAG`) déplace le widget, y compris vers une autre barre des tâches. Le séparateur de gauche disparaît.
- Le flyout se ferme sur Échap, sur perte de focus, ou par un second clic sur le widget.
- Le repère avance chaque minute, la jauge change à chaque poll.
- **Projection** (flyout seulement) : « limite dans ~33 min à ce rythme ». Elle n'apparaît que si la limite serait atteinte avant le reset, et se calcule depuis le même temps écoulé borné que le pace.

## 7. Contraintes
- GDI/GDI+ dans la fenêtre layered existante (`render_layered` / `paint_content` dans `src/window.rs`), badges dans `src/tray_icon.rs`.
- Flyout : nouvelle fenêtre popup, coins arrondis Windows 11 (`DWMWA_WINDOW_CORNER_PREFERENCE`), ancrée au-dessus du widget, dans les limites de l'écran.
- Vérifier que GDI et GDI+ sélectionnent bien les instances « Bahnschrift SemiCondensed » / « Condensed » par nom de famille.
- La refonte « une ligne par provider » non commitée dans `window.rs` est la base de départ ; elle est à réconcilier avant de construire.
