# Icons

`sprite.svg` carries the interface icons of the portal chrome (theme selector, sign out, settings, warning, back). They are
copied verbatim from Tabler Icons v3.49.0, outline set (MIT, Copyright (c) 2020-2026 Paweł Kuna,
https://github.com/tabler/tabler-icons, tag `v3.49.0`, commit `bbed884d15354b5cebf2493371f20dc2d5e83eaf`):
`device-desktop`, `moon`, `sun`, `logout`, `settings`, `alert-triangle`, `chevron-left`. Nothing is drawn by hand, nothing is fetched at runtime.

The sprite has one `<symbol>` per line. Each page inlines only the symbols it draws (no external `<use>`, so no icon request, and no unused node in the page); the login and error pages draw none, the launcher five, the settings pages six. The comment about origin lives here, not in the file, so it is not shipped to the browser.

Service icons are a separate concern (the embedded offline library): see the catalogue documentation.
