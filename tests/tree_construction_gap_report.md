# html5lib Tree Construction Gap Report

> Updated: 2026-07-17 | Suite: html5lib/html5lib-tests `tree-construction/*.test`

## Headline

**Pass rate: 100.0% (1716/1716)** — 204 skipped — 0 panicked

## Per-fixture results

| Fixture | Pass | Fail | Skip | Total |
|---------|-----:|-----:|-----:|------:|
| adoption01.dat | 17 | 0 | 1 | 18 |
| adoption02.dat | 3 | 0 | 0 | 3 |
| blocks.dat | 48 | 0 | 0 | 48 |
| comments01.dat | 16 | 0 | 0 | 16 |
| doctype01.dat | 37 | 0 | 0 | 37 |
| domjs-unsafe.dat | 49 | 0 | 0 | 49 |
| entities01.dat | 75 | 0 | 0 | 75 |
| entities02.dat | 26 | 0 | 0 | 26 |
| foreign-fragment.dat | 0 | 0 | 66 | 66 |
| html5test-com.dat | 24 | 0 | 0 | 24 |
| inbody01.dat | 4 | 0 | 0 | 4 |
| isindex.dat | 4 | 0 | 0 | 4 |
| main-element.dat | 3 | 0 | 0 | 3 |
| math.dat | 0 | 0 | 8 | 8 |
| menuitem-element.dat | 20 | 0 | 0 | 20 |
| namespace-sensitivity.dat | 1 | 0 | 0 | 1 |
| noscript01.dat | 18 | 0 | 0 | 18 |
| pending-spec-changes-plain-text-unsafe.dat | 1 | 0 | 0 | 1 |
| pending-spec-changes.dat | 3 | 0 | 0 | 3 |
| plain-text-unsafe.dat | 33 | 0 | 0 | 33 |
| processing-instructions.dat | 124 | 0 | 0 | 124 |
| quirks01.dat | 4 | 0 | 0 | 4 |
| ruby.dat | 21 | 0 | 0 | 21 |
| scriptdata01.dat | 26 | 0 | 0 | 26 |
| scripted_adoption01.dat | 0 | 0 | 1 | 1 |
| scripted_ark.dat | 0 | 0 | 1 | 1 |
| scripted_webkit01.dat | 0 | 0 | 2 | 2 |
| search-element.dat | 3 | 0 | 0 | 3 |
| svg.dat | 0 | 0 | 8 | 8 |
| tables01.dat | 19 | 0 | 0 | 19 |
| template.dat | 111 | 0 | 1 | 112 |
| tests1.dat | 112 | 0 | 0 | 112 |
| tests10.dat | 54 | 0 | 0 | 54 |
| tests11.dat | 13 | 0 | 0 | 13 |
| tests12.dat | 2 | 0 | 0 | 2 |
| tests14.dat | 7 | 0 | 0 | 7 |
| tests15.dat | 14 | 0 | 0 | 14 |
| tests16.dat | 191 | 0 | 6 | 197 |
| tests17.dat | 13 | 0 | 0 | 13 |
| tests18.dat | 36 | 0 | 0 | 36 |
| tests19.dat | 103 | 0 | 0 | 103 |
| tests2.dat | 63 | 0 | 0 | 63 |
| tests20.dat | 64 | 0 | 0 | 64 |
| tests21.dat | 23 | 0 | 0 | 23 |
| tests22.dat | 5 | 0 | 0 | 5 |
| tests23.dat | 5 | 0 | 0 | 5 |
| tests24.dat | 8 | 0 | 0 | 8 |
| tests25.dat | 26 | 0 | 0 | 26 |
| tests26.dat | 20 | 0 | 0 | 20 |
| tests3.dat | 24 | 0 | 0 | 24 |
| tests4.dat | 0 | 0 | 9 | 9 |
| tests5.dat | 16 | 0 | 1 | 17 |
| tests6.dat | 39 | 0 | 13 | 52 |
| tests7.dat | 33 | 0 | 1 | 34 |
| tests8.dat | 10 | 0 | 0 | 10 |
| tests9.dat | 27 | 0 | 0 | 27 |
| tests_innerHTML_1.dat | 0 | 0 | 81 | 81 |
| tricky01.dat | 9 | 0 | 0 | 9 |
| void-in-phrasing.dat | 13 | 0 | 0 | 13 |
| webkit01.dat | 52 | 0 | 0 | 52 |
| webkit02.dat | 44 | 0 | 5 | 49 |

## Skip reasons

| Count | Reason |
|------:|--------|
| 192 | document-fragment (fragment parsing not implemented) |
| 12 | script-on (scripting flag not implemented) |

## Notes

- **0 failures** across all 68 fixture files
- All 21 insertion modes implemented and passing
- Foreign content (SVG/MathML) fully passing
- Template mode fully passing
- Adoption agency fully passing
- Foster parenting fully passing

## How to run

```bash
cargo test --test html5lib_tree_construction -- --nocapture
```
