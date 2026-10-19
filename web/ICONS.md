# Icons

The file `sprite.svg` contains the interface icons of the portal chrome. These icons are the theme selector, sign out, settings, warning and back icons. The project copies the icons verbatim from Tabler Icons v3.49.0, outline set (MIT, Copyright (c) 2020-2026 Paweł Kuna, https://github.com/tabler/tabler-icons, tag `v3.49.0`, commit `bbed884d15354b5cebf2493371f20dc2d5e83eaf`). The icons are: `device-desktop`, `moon`, `sun`, `logout`, `settings`, `alert-triangle`, `chevron-left`, `chevron-right`. The project does not draw any icon by hand. The portal does not fetch any icon at runtime.

The sprite has one `<symbol>` for each line. Each page inlines only the symbols that it draws. The page does not use an external `<use>`. So there is no icon request, and no unused node in the page. The login and error pages draw no symbols. The launcher draws five symbols. A settings page draws only the symbols that it uses, seven at most. The comment about origin is in this file, not in `sprite.svg`. So the browser does not receive the comment.

Service icons are a separate subject. They are in the embedded offline library. Refer to the catalogue documentation.
