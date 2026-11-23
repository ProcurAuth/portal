package app.portalinbox.portal

import app.portalinbox.data.Server
import app.portalinbox.service.KeyChange
import app.portalinbox.service.LinkState
import app.portalinbox.service.LinkStatus
import app.portalinbox.ui.VisibleText

/**
 * The model of one row of the portal list and of the head of the portal screen. Pure Kotlin: the screen
 * gets the texts it can format without a decision.
 */
data class PortalRow(
    val identity: PortalIdentity,
    /** The host as a person reads it, with the port when the address has one. No scheme. */
    val host: String,
    val login: String,
    /** The first three groups of the fingerprint: `a3f9 07c2 5be1`. */
    val keyPrefix: String,
    /** True during the first 24 hours: the "New" mark. */
    val isNew: Boolean,
    val state: LinkState,
    /** When the state began (phone clock). The offline age uses it. */
    val stateSinceMs: Long,
    /** The detail that the connection gives for the state, or null. */
    val stateDetail: String?,
    /** The time of the last frame (phone clock), or null when the portal never connected. */
    val lastSyncMs: Long?,
    val openCount: Int,
    /** What the portal calls itself, made safe to show, and only when it differs from the nickname. Null otherwise. */
    val reportedName: String?,
) {
    companion object {
        const val KEY_PREFIX_GROUPS = 3

        /** [status] is null when the registry knows nothing yet: the state is "connecting" unless the row holds a reason to stop. */
        fun of(server: Server, status: LinkStatus?, openCount: Int, nowMs: Long): PortalRow {
            val identity = PortalIdentity.of(server)
            // A stored reason stops the connection: it shows even before the connection thread sets the status.
            val state = status?.state ?: KeyChange.stateOf(server.reenrollReason) ?: LinkState.CONNECTING
            return PortalRow(
                identity = identity,
                host = hostOf(server.address),
                login = server.userName,
                keyPrefix = identity.shortFingerprint(KEY_PREFIX_GROUPS),
                isNew = PortalIdentity.isNew(server.addedAtMs, nowMs),
                state = state,
                stateSinceMs = status?.sinceMs ?: nowMs,
                stateDetail = status?.detail,
                lastSyncMs = server.lastSyncMs,
                openCount = openCount,
                reportedName = reportedNameIfDifferent(server.nickname, server.reportedName),
            )
        }

        /** The host of [address] as a person reads it (Punycode decoded), with an explicit port. */
        fun hostOf(address: String): String = EnrollPlan.shownAddress(address).substringBefore('/')

        /**
         * The name that the portal gives itself, for the line "The portal calls itself ...". It is the text of the portal, so
         * hidden characters are made visible. Null when it is empty or the same as the nickname (without case).
         */
        fun reportedNameIfDifferent(nickname: String, reported: String): String? {
            val text = reported.trim()
            if (text.isEmpty() || text.equals(nickname.trim(), ignoreCase = true)) return null
            return VisibleText.escapeLine(text)
        }
    }

    /** True when nothing can be sent to this portal until the user acts: key changed, or a revoked device. */
    val needsUser: Boolean get() = state == LinkState.KEY_CHANGED || state == LinkState.REENROLL
}
