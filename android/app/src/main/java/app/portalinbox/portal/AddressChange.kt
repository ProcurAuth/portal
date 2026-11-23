package app.portalinbox.portal

import app.portalinbox.net.ServerAddress

/**
 * The rules of "Change address". Pure Kotlin. A portal has one address. The pinned key does not
 * change: it still decides who can answer. A new address that leads to another key fails the handshake and shows "Key changed".
 * Two addresses of one portal are not kept: the second would be a second enrollment of one key.
 */
object AddressChange {
    sealed interface Result {
        /** [http] is the normalized address to store. */
        data class Ok(val http: String) : Result

        /** The text is not an address. */
        object Invalid : Result

        /** The address is the one that the portal has now: nothing to do. */
        object Same : Result

        /** Another portal already has this address. The screen refuses, because the second portal would hold one key at two addresses. */
        data class UsedBy(val portal: KnownPortal) : Result
    }

    /** [portal] is the portal to change. [existing] are all portals of the phone, with the portal itself. */
    fun check(raw: String, portal: KnownPortal, existing: List<KnownPortal>): Result {
        val parsed = ServerAddress.parse(raw) ?: return Result.Invalid
        val mine = LookAlike.parts(parsed.http) ?: return Result.Invalid
        if (ServerAddress.parse(portal.address)?.http == parsed.http) return Result.Same
        existing.firstOrNull { it.identity.id != portal.identity.id && LookAlike.parts(it.address) == mine }
            ?.let { return Result.UsedBy(it) }
        return Result.Ok(parsed.http)
    }
}
