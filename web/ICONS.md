# Icons

The file `sprite.svg` holds the interface icons of the portal. The directory `icons-ui/` holds the original files of eight of them.

## Origin and licence

The icons are copies of the outline set of Tabler Icons v3.49.0. The tag is `v3.49.0` and the commit is `bbed884d15354b5cebf2493371f20dc2d5e83eaf`.
The source is https://github.com/tabler/tabler-icons. Tabler Icons is under the MIT licence. The notice follows.

```text
MIT License

Copyright (c) 2020-2026 Paweł Kuna

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

The project draws no icon by hand. The portal fetches no icon at run time.

## The symbols

The sprite has one `<symbol>` for each icon. These are the names:

```text
device-desktop, moon, sun, logout, settings, alert-triangle, chevron-right, arrow-left, refresh, plus, trash, copy, lock, lock-open, x, ban, check, folder, pencil, chevron-down, bell, bell-ringing, key, device-mobile, device-mobile-off, clock, circle-check, search, send, book, user-share, activity, inbox, webhook, robot, user, users, help-circle, checkbox, point, tags, eye, eye-off, shield-check, arrow-up-right, git-merge
```

Each page inlines only the symbols that it draws. The page has no external `<use>`, so a page needs no icon request.

- The error pages draw no symbol.
- The sign-in page draws `lock`, `alert-triangle`, `eye` and `eye-off`. The card of the second step draws the first two.
- The launcher draws four symbols, and five with the gear. A tile of an external service adds `arrow-up-right`, which marks the badge "external".
- An account that receives notifications adds `inbox`, `x`, `alert-triangle`, `bell` and `bell-ringing`. The settings pages add the same toast symbols.
- The editor of permissions adds `lock-open`. With `lock`, it shows the state of the approval of a permission.
- The editor also adds `shield-check`, which marks a service that publishes its permissions.

Service icons are a separate subject. They are in the embedded offline library. See [Launcher and service discovery](../docs/launcher.md).

The sprite has no symbol of the logo. No page body shows the logo. The logo is the icon of the tab, in `favicon.svg`.
