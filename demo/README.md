# Local demo

This demo runs the portal with Traefik and three example services on one machine. It uses Docker only. Nothing goes to a registry.

## Start and stop

1. Run `./demo/up.sh`. It builds the portal image, makes the secrets, and starts the stack.
2. Open `http://localhost:8088/`.
3. Read the logins and the passwords in `demo/.secrets/credentials.txt`.
4. To stop the demo, run `docker compose -f demo/docker-compose.yml down -v`.

The stack has these parts:

| Part | Role |
|---|---|
| `traefik` | The edge. The port 8088 is open on loopback only. |
| `portal` | The portal: sessions, the PDP on port 4182, the edge for keys. |
| `notes` | A service with a front. It asks the PDP for each operation. |
| `files`, `wiki` | Two example services. They show the request headers. |

To check the main points from a terminal, run `./demo/smoke.sh`. It signs in, tests the gate, a ceiling, a budget and a key. The budget of `alice` is 5 uses each minute, so wait one minute before a second run.

## Accounts

| Account | Source | Grants |
|---|---|---|
| `admin` | superadmin | Every right. No limit. |
| `alice` | file | `access` on notes, files and wiki. Read of all notebooks, with at most 2 notes in one read. Write and delete in the notebook `work`. A budget of 5 uses each minute on notes. |
| `bob` | file | `access` on notes and wiki. Read of the notebook `public` only. |

The superadmin can add users in the settings page. The superadmin can also make keys on the page `/auth/keys`. The button **New key** opens the page `/auth/keys/new`. The service **ClickHouse Dash** looks like ChDash: three hosts, their databases (listed by the service, page by page) and the actions of ChDash. The service **Big database** publishes 424 actions and 220 resources. Use it to try the editor of grants and the tables with a large action document.

## Tests to do

1. **Launcher.** Sign in as `bob`. The launcher shows notes and wiki, not files. Open `/files/` by hand: the portal refuses.
2. **Grants on nodes.** Open Notes as `bob`. Only the notebook `public` has the button Read. Sign in as `alice`: all notebooks can be read, and only `work` can be written.
3. **Ceiling.** As `alice`, open a notebook that has 3 notes. The list shows 2 notes. The ceiling `max_results` is 2.
4. **Budget.** As `alice`, press "Read 8 times". The first reads give 200, and then the answer is 429 with `Retry-After`. The budget is 5 uses each minute. Wait for the window to end, and read again.
5. **PDP playground.** Ask `notes.read` on `notebook:work/note:1`. Ask `notes.read` on `notebook:work` with `subtree`. Ask a resource that the tree does not have, for example `folder:x`: the answer is `invalid_resource`.
6. **Users page.** As `admin`, open the settings, then Users. Add a user and give permissions with limits. Sign in as this user in a private window and look at the effect.
7. **Keys.** As `admin`, open `/auth/keys` and click **New key** and make a key with the grants `access on service:notes` and `notes.read on service:notes`. The new key is a page of its own. Copy the text. Paste it in "Use a key" in Notes. The key acts for its owner, and it never gets more than its owner.
8. **Key from a terminal.** Run `curl -i -H "Authorization: Bearer mcp_..." http://localhost:8088/notes/api/notebooks`. A bad key gives 401 with an empty body and the header `WWW-Authenticate`.
9. **Revoke.** Revoke the key on the page. The next call gives 401 within seconds.
10. **Limits on a key.** Make a key with a budget on `access on service:notes`. The counters of the key and of its owner both count each use.
11. **Services page.** As `admin`, open the page Services. It shows the document of notes with its tree, its actions and its limits.
12. **Logs.** Run `docker logs -f portal-demo`. The log has one line for each decision of the PDP and one line for each failed key check.

## Files

- `docker-compose.yml`: the stack.
- `portal.hcl`: the configuration of the portal. It has the block `keys {}`, the PDP, and the two accounts of the file.
- `traefik-dynamic.yml`: the middlewares for persons and for keys.
- The portal image is `docker/Dockerfile` of the repository. `up.sh` builds it as `local/portal:demo`.
- `smoke.sh`: the checks from a terminal.
- `notes/`: the service Notes (`app.py`, `index.html`).
