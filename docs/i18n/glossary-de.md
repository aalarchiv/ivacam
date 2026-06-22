# German glossary (CAM terminology)

Authoritative German terminology for the `de.json` catalog. Per the epic
decision (`docs/i18n/PLAN.md`): **English base = plain English; German = the
conventional German machinist wording.** Translators MUST use these terms so
the German UI matches what German CNC users already expect.

These are standard German CAM/CNC trade terms. Keep them consistent across the
whole catalog; when a term here applies, do not invent a synonym.

## Operation kinds

| English | German |
| --- | --- |
| Profile | Profilieren |
| Pocket | Tasche |
| Drill | Bohrung |
| Engraving | Gravur |
| Thread | Gewinde |
| Chamfer | Anfasen |
| Part | Teil |
| Hole | Ausschnitt |
| Carve | Carve |
| Drag knife | Schleppmesser |
| Tapered (V-carve) | Kegel |

## Strategy / passes

| English | German |
| --- | --- |
| Roughing | Schruppen |
| Finishing | Schlichten |
| Conventional milling | Gegenlauf |
| Climb milling | Gleichlauf |
| Trochoidal / whirling | Wirbeln (Wirbelbreite, Wirbeloszillation, Wirbelzustellung) |
| Depth per pass | Tiefenzustellung |
| Order | Reihenfolge |
| Direction | Richtung |

## Feeds & speeds

| English | German |
| --- | --- |
| Feedrate | Vorschubgeschwindigkeit (Vorschub) |
| Plunge feedrate | Eintauchgeschwindigkeit |
| Plunge angle | Eintauchwinkel |
| RPM | Drehzahl |
| Spindle | Fräsmotor |

## Geometry & toolpath

| English | German |
| --- | --- |
| Depth | Tiefe |
| Diameter | Durchmesser |
| Offset | Versatz |
| Tab / holding tab | Anbinden |
| Lead-in | Lead-in |
| Lead-out | Lead-out (loanword, confirmed usable in German) |
| Inside | innen |
| Outside | außen |
| Center | Zentrieren |
| Angle | Winkel |
| Radius | Radius |
| Start point | Startpunkt |

## Tooling

| English | German |
| --- | --- |
| Profiling tool | Profilfräser |
| Chamfering tool | Fasenfräser |
| Threading tool | Gewindefräser |
| Tapered tool | Kegelfräser |
| Tool diameter | Durchmesser |

## Material / stock

| English | German |
| --- | --- |
| Material thickness | Materialdicke |
| Stock / raw material | Rohmaterial |
| Layer | Layer (Layerliste) |

## Common UI

| English | German |
| --- | --- |
| OK | OK |
| Cancel | Abbrechen |
| Delete | Löschen |
| Name | Name |
| View | Ansicht |
| Mode | Modus |
| Automatic | Automatik |
| Manual | Manuell |
| Homing | Referenzfahrt |
| Save project | Projekt speichern |

## Units

| English | German |
| --- | --- |
| mm | mm |
| inch | Zoll |
| Zero | Null |

## Uncertain — needs native-CNC-user confirmation

Terms our UI uses that have **no direct or unambiguous** German trade term.
Flag in `QNA_I18N.md` for a German-speaking CNC user to confirm rather than
guessing:

- Ramp / helical ramp entry
- Stock box / stock outline (3D preview)
- Snap-to targets (grid, endpoint, midpoint…)
- App-shell / settings vocabulary with no CAM analogue (translate as plain
  German, not machinist jargon)
