# Traefik and TLS

portal knows nothing about TLS. Traefik terminates TLS and calls portal for each request that needs a decision. This page lists the duties of the deployment in Traefik. They are the routes, the two ForwardAuth middlewares, the header rules and the recipes for the routes of the agents.

## The model

The listeners of portal speak plain HTTP. Traefik terminates TLS at the entry point and sends plain HTTP to portal on a private network. Traefik uses portal in three ways:

- **ForwardAuth for persons:** Traefik calls `/auth/verify` before each request to a protected service (see [Grants and the gate](grants-and-gate.md#the-gate)).
- **ForwardAuth for keys:** Traefik calls `/auth/verify-key` before each request of an agent (see [Keys and limits](keys-and-limits.md#the-pages-and-the-edge)).
- **Plain routes:** Traefik sends the pages of portal, the routes of the phones, the senders and the agents to portal. These routes have no ForwardAuth, because they have their own checks.

The examples are in `examples/` and in `demo/traefik-dynamic.yml`. The Compose stack of `demo/` runs all of them (see [the quick start](getting-started.md)).

## Set-up steps

Run portal in this way:

1. Publish only Traefik. Do not publish the ports of portal. The PDP port is for the services only.
2. Make the ForwardAuth middleware for persons, for `/auth/verify`. It copies `Cookie` and `User-Agent` to portal and the identity token back. Set it on the entry point, or on each service router.
3. Route `/auth/` and `/` to portal. Do not apply ForwardAuth to `/auth/`: portal checks the session itself.
4. Route `/v1/`, `/api/v1/`, the path of the MCP endpoint (`inbox.mcp.path`, default `/mcp`) and `/api/version` to portal without ForwardAuth. Add a retry middleware. A phone, a sender and an agent have no session.
5. For key routes of agents, use a second ForwardAuth middleware that points to `/auth/verify-key` and copies `Authorization`. Chain the middleware `drop-authorization` after it.
6. Set `server.trusted_proxies` to the network of Traefik.
7. Keep `trustForwardHeader: false` on each ForwardAuth middleware.

### The names of the middlewares

| Job | Name in `demo/traefik-dynamic.yml` | Name in `examples/traefik-dynamic.yaml` |
|---|---|---|
| ForwardAuth for persons | `portal-user` | `portal` |
| ForwardAuth for keys | `portal-key` | `portal-key` |
| Remove the key before the service | `drop-authorization` | `drop-authorization` |
| The chain for key routes | `portal-key-chain` | `portal-key-chain` |
| Retry for the routes of the Inbox | `inbox-retry` | none |

The names are free. This page uses the names of the demo.

### The key must not reach the service

The answer of portal cannot clear a header. Only the proxy can remove the key before the service (protocol 9.1). Chain the middleware `drop-authorization` after `portal-key`. In the examples this chain is `portal-key-chain`. Without `drop-authorization`, the key reaches the service. portal cannot see the proxy, so this is a duty of the deployment.

### The retry of the Inbox routes

The middleware `inbox-retry` sends a request again when portal does not answer at all. Examples are a lost node and a refused connection. The demo uses 4 attempts, 100 ms apart at first. Traefik retries only before the first byte of the answer. A WebSocket upgrade that got `101` is never sent again. An answer of portal, also a `503`, is final.

### Trusted proxies and forwarded headers

- `server.trusted_proxies` is mandatory. It lists the networks of the proxies that set `X-Forwarded-For`. portal reads this header only from a TCP peer in these networks. It then takes the first address from the right that is not in these networks. The client address keys the session binding, the sign-in limiter and the limiter of the enrollment attempts of the phones.
- portal reads `X-Forwarded-Proto` and `X-Forwarded-Host` as they arrive. With `trustForwardHeader: false`, Traefik writes them from the real request, so a client cannot choose them. No other pod may reach port 4181.
- `X-Forwarded-Uri` and `X-Forwarded-Method` follow the same rule. Traefik writes them from the real request.

Set `trustForwardHeader: true` only when each hop before Traefik overwrites or removes the `X-Forwarded-Uri` and `X-Forwarded-Method` headers of the client. Otherwise the client chooses the request that `/auth/verify` checks. For example, a request with `X-Forwarded-Uri: /auth/login` passes the gate without a session.

Behind a TCP load balancer, give the client address with the PROXY protocol of the entry point. Behind an HTTP proxy, keep `trustForwardHeader: false` and set `session.bind = "user_agent"`.

## A global middleware and a key route exclude each other

A middleware on the entry point runs on each router of this entry point, also on a key route. A request that has only `Authorization: Bearer mcp_...` has no session. The check `/auth/verify` then refuses it before the key route sees it. The priority of a router does not change this. Use one of these two recipes:

- **A dedicated entry point for the agents.** Add an entry point with no middleware, for example `websecure-agents` on its own port. Put the key routers on it, with the middleware `portal-key-chain`. Keep the middleware for persons on the entry point of the persons. The example `examples/traefik-entrypoint-snippet.yaml` shows both entry points.
- **No middleware on the entry point.** Put the middleware for persons on each router of a person. Put `portal-key-chain` on each key router. A key router has a rule that only a key matches, for example ``PathPrefix(`/clickhouse/mcp`) && HeaderRegexp(`Authorization`, `^Bearer mcp_`)``. Give it a higher priority than the router of the persons for the same path.

portal ignores the cookies on a key route. A request on this route needs a key. A route for keys is never in `portal.exclude`.

## HTTPS and the cookies

HTTPS gives the session cookie its `__Host-` name and its `Secure` attribute. The scheme comes from `X-Forwarded-Proto`: only the single value `https` counts as HTTPS. Over plain HTTP, the cookie has no prefix and no `Secure` attribute.

A phone and a sender send secrets in clear text on plain HTTP. Use an HTTPS `inbox.devices.public_address` and an HTTPS URL for the senders. See [Security model](security.md) for the other rules.

## Global ForwardAuth

The project is designed for a middleware at the level of the entry point. A request to a protected application first reaches `/auth/verify`. Without a valid session, portal returns a redirect to `/auth/login`. The redirect has a validated path and query as the return target. It is signed with an HMAC. After a successful sign-in, portal returns the browser to that target.

`/auth/verify` explicitly lets the public paths of the sign-in pass. These paths are `/auth/login`, `/auth/logout`, the two steps of the second factor and the style and script files of the sign-in pages. This prevents a loop when the ForwardAuth middleware is global. `/auth/success` still needs a valid session.

Any other `GET` path that portal receives redirects to the sign-in page when there is no session. This applies to `/auth/` and to the catch-all route. It applies whether or not the path exists. When the user has a session, portal answers `404`.

In the K3s routes of `deploy/k3s/ingress.yaml`, the catch-all route runs ForwardAuth first. Then Traefik rewrites the path to `/__portal-fallback-404`. This keeps the endpoints of portal off arbitrary paths. The route of the Inbox in the same file matches `/v1/`, `/api/v1/`, `/mcp` and `/api/version` with a priority of 50000.

See also:

- `examples/traefik-entrypoint-snippet.yaml`
- `examples/traefik-dynamic.yaml`
- `demo/`: a complete local stack with Traefik, portal, a service with a front, and the routes for keys (see `demo/README.md`)

## Absolute paths of scripts and styles

The authentication cookie has `Path=/`. So same-origin resources carry the same session cookie. Examples are `/app.js`, `/css/app.css`, `/api/...` and `/images/...`. After sign-in, these resources pass the global ForwardAuth check.

Authentication does not rewrite application paths. Suppose that an application mounted under `/testservice` emits `/app.js`. Traefik still needs a rule that maps `/app.js` to the intended backend.

## Related pages

[Security model](security.md) gives the invariants of the gate and the other rules of the deployment. [Inbox: security model](inbox/SECURITY.md) gives the model of the Inbox.
