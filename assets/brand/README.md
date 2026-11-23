# Brand files of portal

This folder holds the logo of portal. The logo is the Doorway: a heavy arch with a cyan dot in the doorway.

## Files

| File | Use |
|---|---|
| `logo-lockup.svg` | The mark and the word `portal`. Signal Blue mark, deep cyan dot, navy word. Use it on a white or light page. |
| `logo-lockup-onblue.svg` | The same lockup in white, with the cyan dot. Use it on Signal Blue or on a dark page. |
| `logo-mark.svg` | The mark alone, in full colour, for a white page. |
| `logo-icon.svg` | The mark on a rounded tile of Signal Blue. This is the icon of the tab of the browser (`web/favicon.svg` is a copy). |
| `avatar-500.png` | The avatar of 500 by 500 pixels. It is the mark on a square of Signal Blue. |
| `social-preview-1280x640.png` | The social card of 1280 by 640 pixels. It is the white lockup on Signal Blue. |

The SVG files have no font, no script and no external reference. The word is a set of paths.
The two PNG files come from the SVG files. Render them again with a browser at the exact size and a scale of 1.

## Upload by hand

GitHub does not read the avatar or the social card from the repository. Upload them in the settings.

1. Open the settings of the repository or of the organisation.
2. For the avatar, upload `avatar-500.png`.
3. For the social card, open the settings of the repository and upload `social-preview-1280x640.png`.

## Colours

| Name | Value | Use |
|---|---|---|
| Signal Blue | `#1238F0` | The primary colour. Primary fills, the mark on a light page, the tile. White text on it has a ratio of 7.4:1. |
| White | `#FFFFFF` | The mark and the word on Signal Blue or on a dark page. |
| Cyan | `#7FF0FF` | The dot on Signal Blue and on a dark page. |
| Deep cyan | `#38B6FF` | The dot on a white or light page. |
| Tint 300 | `#7D98FF` | Accent text and thin marks on a dark surface. |
| Tint 200 | `#BFD0FF` | A light blue for large areas on a dark surface. |
| Tint cyan | `#CFF8FF` | A light cyan for large areas. |
| Tint 50 | `#EEF2FF` | A very light blue for a light surface. |
| Ink | `#0B1220` | Text on a light surface. |
| Navy | `#03061A` | The word of the lockup on a white page. |
| Deep navy | `#0A1A6B` | A dark blue for large areas. |

The interface of the portal uses these colours as tokens. See [Settings pages and history](../../docs/settings-and-history.md#brand-tokens-and-logo).

## Typeface of the word

The word is Unbounded at weight 600. The letter space is -0.03 em. The word is outlined to paths.

Unbounded is a family of Google Fonts. Its publisher gives it under the SIL Open Font License 1.1.

The SVG files carry the outlines of the glyphs and not the font. This repository holds no font file, so it holds no copy of the licence text.

The licence does not restrict a logo that is made from outlines. Read the licence at the source of the family before you make a font file from it.

## Rules of use

1. Keep the clear space. The space on each side of the lockup is at least the width of one leg of the arch.
2. Keep the minimum size. The lockup is at least 80 pixels wide. The mark alone is at least 16 pixels wide.
3. Do not change the colours. Use the colours in the table above only.
4. Do not stretch, rotate or outline the logo. Do not add an effect to it.
5. Do not draw the dot out of the doorway and do not remove it.
6. Use the white form on a dark or blue background. Use the full-colour form on a white background.
7. Do not show the logo in the body of a page of the product. The logo is the icon of the browser tab only.
8. The key `ui.favicon` controls the icon of the tab. The value `"default"` shows the icon on the pages after sign-in. The value `"none"` removes it. The sign-in pages and the error pages never show it.
9. Do not show the name `portal` in the body of a page. The name is in the title of the tab only, as the value of `server.name`.

These rules are for the pages that the server sends. The README of the repository and the files of this folder may show the lockup.
