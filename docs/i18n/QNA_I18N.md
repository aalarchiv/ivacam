# QNA — German terminology to confirm

Auto-flagged during the de.json translation (ivac-os2k.8). Each row is a string
where the translator was unsure of the exact German trade term. **A German-speaking
CNC user: confirm or correct the German.** Fold any change back into
`docs/i18n/glossary-de.md` and `frontend/src/lib/i18n/messages/de.json`.

Total flagged: **33**.


## `settings`

| Key | English | Current German | Note |
| --- | --- | --- | --- |
| `settings.group.snap` | Snap to | Fangen auf | Snap-to terminology has no standard German term (glossary flags it as uncertain). 'Fangen auf' reads as a group header preceding Endpunkt/Mittelpunkt; an the reference user may prefer 'Fangen' or 'Einrasten'. Needs confirmation. |
| `settings.view.show_stock_box` | Show stock outline in 3D | Rohmaterialumriss in 3D anzeigen | 'Rohmaterial' matches glossary; 'stock outline' is flagged uncertain in glossary (no direct standard German term). 'Rohmaterialumriss' is a sensible coinage but should be confirmed. |
| `settings.snap.midpoint.help` | Snap to the midpoint of each segment. | Auf den Mittelpunkt jedes Segments einrasten. | standard German uses 'einrasten' for snap (not 'fangen'); glossary flags snap-targets as needing the reference-user confirmation. |
| `settings.snap.intersection.help` | Snap to line / arc crossings. | Auf Linien-/Bogen-Schnittpunkte einrasten. | 'einrasten' per reference vocabulary; snap-target term flagged uncertain in glossary. |
| `settings.snap.center.help` | Snap to circle / arc centers. | Auf Kreis-/Bogen-Mittelpunkte einrasten. | 'einrasten' per the reference; 'Mittelpunkt'/'Kreismitte' is the geometry sense (glossary 'Zentrieren' is the zeroing action, not applicable here). |
| `settings.snap.hint` | Object snap on the 2D canvas. Endpoint / midpoint / intersection / center latch the curs… | Objektfang auf der 2D-Zeichenfläche. Endpunkt / Mittelpunkt / Schnittpunkt / Zentrum ras… | Endpoint/Center keys not in this chunk; kept 'Endpunkt'/'Zentrum' to mirror the visible snap-target labels. German wording is 'einrasten'/'Raster'; full snap-target vocabulary flagged for the reference-user confirmation per glossary. |

## `ops`

| Key | English | Current German | Note |
| --- | --- | --- | --- |
| `ops.kind.probe` | Probe | Antasten | standard German uses 'Taster'/'Sensor' for the device and 'Antasten' for touch-off (e.g. 'X/Y/Z Antasten'). 'Antasten' chosen for the operation verb; no standalone probe-op menu label in the reference vocabulary to confirm exactly. |
| `ops.help.relief_mill` | 3D relief surfacing from a grayscale image with a ball-nose cutter. Brightness becomes h… | 3D-Reliefbearbeitung aus einem Graustufenbild mit einem Kugelfräser. Helligkeit wird zu … | 'scallop' rendered as 'Restmaterialhöhe' (scallop/cusp height); no exact standard German term in catalog — confirm with expert. |
| `ops.drill.spot_options` | Spot options | Anbohr-Optionen | 'Spot' = spot drilling/Anbohren; no direct reference vocabulary entry to confirm exact wording. |
| `ops.pocket.strategy.halfpipe` | Halfpipe (slot, profiled floor) | Halfpipe (Nut, profilierter Boden) | "Halfpipe" hat kein standard German term; als Loanword beibehalten — vom CNC-Fachleuten bestätigen lassen |
| `ops.raster_engrave.overscan.label` | Overscan | Überlauf | "Overscan" nicht im reference vocabulary; "Überlauf" als beschreibende Übersetzung gewählt — CNC-Fachleuten sollte bevorzugten Begriff bestätigen (auch "Überfahren" möglich). |
| `ops.relief_mill.scallop.label` | Scallop | Restkamm | "Scallop" hat keine standard German term; "Restkamm" als gängiger CAM-Begriff gewählt (auch "Kammhöhe" möglich). Vom CNC-Fachleuten bestätigen lassen. |
| `ops.drill.peck_step.label` | Peck step | Bohrschritt | 'Peck step' = Zustellung pro Bohrstich. standard German nutzt 'Tiefenzustellung' für den Tiefenzuwachs beim Bohren und 'Spanbrechen' für peck; kein direkter Begriff für 'peck step'. CNC-Fachleuten bestätigen. |
| `ops.vcarve.mode.perimeter` | Perimeter | Kontur | „Perimeter“ (V-Carve-Modus) hat keinen direkten standard German term; „Kontur“ gewählt im Sinne von Randverfolgung. CNC-Fachleuten sollte bestätigen. |

## `menu`

| Key | English | Current German | Note |
| --- | --- | --- | --- |
| `menu.show_regions` | Show regions | Bereiche anzeigen | "Regions" = translucent fill marking each pocket's machined region (CAM-view feature, no standard German term). "Bereiche" is a reasonable plain-German rendering; confirm preferred term with native CNC user. |

## `dialog`

| Key | English | Current German | Note |
| --- | --- | --- | --- |
| `dialog.export_stl.no_stock` | No simulated stock to export — run Generate first. | Kein simuliertes Rohmaterial zum Exportieren – führen Sie zuerst Generieren aus. | 'Generate' refers to the genbar Generate-G-code button, which is not yet translated in de.json. the reference's analogous action is 'Berechnen'/'CNC-Programm erzeugen'. Confirm the German button label and make this match it (likely 'Generieren' or 'Berechnen'). |

## `tools`

| Key | English | Current German | Note |
| --- | --- | --- | --- |
| `tools.file.save.inventory.title` | Export the shop inventory (as shown, including unapplied edits) to a .ivac-toolset.json … | Werkstattbestand (wie angezeigt, einschließlich nicht übernommener Änderungen) in eine .… | "shop inventory" → Werkstattbestand: kein standard German term vorhanden; vom CNC-Fachleuten bestätigen lassen (muss zu machinetool.shop_inventory konsistent sein). |
| `tools.col.tip_angle.title` | Full apex angle for V-bits / engravers — drives V-Carve depth. | Voller Spitzenwinkel für V-Fräser / Graviermesser — bestimmt die V-Carve-Tiefe. | "V-bit" hat keinen direkten standard German term (hier V-Fräser); Spitzenwinkel für "apex angle". Vom CNC-Fachleuten bestätigen lassen. |
| `tools.holder.stickout` | Stickout (mm) | Auskraglänge (mm) | Stickout: kein standard German term; übliches Maschinenwort „Auskraglänge“ verwendet. |
| `tools.compression` | Compression | Kompressionsfräser | No standard German term for compression/up-down cutter; rendered as standard German trade term 'Kompressionsfräser'. |
| `tools.bullnose` | Bull-nose | Radiusfräser | Bull-nose endmill; common German trade term is 'Radiusfräser' (Eckenradius-/Torusfräser). Not in glossary. |

## `opprops`

| Key | English | Current German | Note |
| --- | --- | --- | --- |
| `opprops.tool.title.relief` | Ball-nose tool from the project library — its radius drives the scallop + the surface fo… | Kugelfräser aus der Projektbibliothek — sein Radius bestimmt die Restrauheit und die Flä… | 'scallop' (Restrauheit/Auskolkung) und 'surface follow' (Flächennachführung) sind 3D-Oberflächenbegriffe ohne standard German term — vom CNC-Fachleuten bestätigen lassen |
| `opprops.cut.through_depth` | Through depth | Durchbruchtiefe | No direct standard German term for 'through depth'; 'Durchbruchtiefe' coined for cutting past nominal depth on through-cuts. Needs native CNC user confirmation. |
| `opprops.feeds.corner_slow.title` | Slow the feed at sharp Line→Line corners by this fraction. 0 = no reduction (default). 0… | Verringert den Vorschub an scharfen Linie→Linie-Ecken um diesen Anteil. 0 = keine Verrin… | "Corner slow" hat keinen direkten standard German term; "Eckenverlangsamung" als beschreibende Bildung gewählt — vom CNC-Fachleuten zu bestätigen. |

## `toolform`

| Key | English | Current German | Note |
| --- | --- | --- | --- |
| `toolform.form_profile.title` | Form / profile cutter cross-section (cove / ogee / dovetail / T-slot / custom). The (z, … | Querschnitt eines Form-/Profilfräsers (Hohlkehle / Karnies / Schwalbenschwanz / T-Nut / … | cove=Hohlkehle, ogee=Karnies — Fräsprofilbegriffe, nicht im the reference-Glossar; gängige deutsche Fachbegriffe verwendet. Bitte vom CNC-Fachleuten bestätigen lassen. |

## `machinews`

| Key | English | Current German | Note |
| --- | --- | --- | --- |
| `machinews.tooling` | Tooling | Bestückung | "Tooling" als Bestückung der Maschine mit Werkzeugen; kein direkter standard German term — vom CNC-Fachleuten zu bestätigen. |

## `playback`

| Key | English | Current German | Note |
| --- | --- | --- | --- |
| `playback.scrub.exact` | Scrub: the 3D heightfield exactly tracks the playhead — backstep restores the nearest ch… | Scrubbing: Das 3D-Höhenfeld folgt exakt der Abspielposition – ein Schritt zurück stellt … | "Scrubbing" und "Höhenfeld" sind nicht im Glossar (Lehnwort/Neubildung). Der zitierte Label „Exaktes 3D-Zurückspulen“ muss mit der finalen Übersetzung von settings.performance.exact_rewind ("Exact 3D rewind on backstep") übereinstimmen — CNC-Fachleuten bitte bestätigen. |
| `playback.scrub.forward` | Scrub: the 3D heightfield is forward-only — backstep is a no-op for the sim. Cells retai… | Scrubbing: Das 3D-Höhenfeld läuft nur vorwärts – ein Schritt zurück bleibt für die Simul… | "Scrubbing"/"Höhenfeld" Neubildung; zitierter Label „Exaktes 3D-Zurückspulen“ muss mit settings.performance.exact_rewind übereinstimmen. Konsistent mit playback.scrub.exact gehalten. |

## `textlist`

| Key | English | Current German | Note |
| --- | --- | --- | --- |
| `textlist.field.letter_gap` | Letter gap | Zeichenabstand | standard German uses 'Zeichenabstand' for 'Char spacing' / additional distance between characters; changed from 'Buchstabenabstand'. Confirm with native CNC user. |

## `oplist`

| Key | English | Current German | Note |
| --- | --- | --- | --- |
| `oplist.repick` | Re-pick | Neu zuordnen | "Re-pick" hat keine direkte standard German term; "Neu zuordnen" als Annäherung gewählt — vom CNC-Fachleuten bestätigen lassen. |

## `app`

| Key | English | Current German | Note |
| --- | --- | --- | --- |
| `app.save.carved_stl` | Save carved STL | Gefrästes STL speichern | "carved STL" hat keine direkte standard German term; the reference lässt "Carve" unübersetzt — "gefrästes STL" als natürliche Wiedergabe des Simulationsergebnisses gewählt, vom Experten zu bestätigen |
| `app.viewport.threed_tip` | Click to switch to 3D. Click again to cycle preview mode: both → wireframe → solid. Shif… | Klicken, um zu 3D zu wechseln. Erneut klicken, um den Vorschaumodus zu durchlaufen: beid… | 'Drahtgitter' (wireframe) and 'Volumen' (solid) have no the reference binding — these are 3D-preview terms ours; confirm preferred wording with native CNC user. |

## `calib`

| Key | English | Current German | Note |
| --- | --- | --- | --- |
| `calib.result` | Wear offset: <strong>{wear} mm</strong> — toolpaths will cut as a <strong>{effective} mm… | Verschleißversatz: <strong>{wear} mm</strong> — Werkzeugwege werden mit einem Werkzeug v… | „Wear offset“ hat keinen direkten standard German term; „Verschleißversatz“ ist Standard-Maschinen-Deutsch (standard German nutzt „Versatz“ für Offset, „Verschleiß“ für wear). Von CNC-Fachleuten bestätigen lassen. |
