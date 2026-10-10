# Icons

`sprite.svg` carries the interface icons of the portal chrome (theme selector, sign out, settings, warning, close). They are
copied verbatim from Tabler Icons v3.49.0, outline set (MIT, Copyright (c) 2020-2026 Paweł Kuna,
https://github.com/tabler/tabler-icons, tag `v3.49.0`, commit `bbed884d15354b5cebf2493371f20dc2d5e83eaf`):
`device-desktop`, `moon`, `sun`, `logout`, `settings`, `alert-triangle`, `x`. Nothing is drawn by hand, nothing is fetched at runtime.

The sprite is inlined once in each page (no external `<use>`), so the page needs no icon request.

Service icons are a separate concern (the embedded offline library): see the catalogue documentation.
