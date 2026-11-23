package app.portalinbox.portal

import app.portalinbox.data.Server
import app.portalinbox.service.LinkState
import app.portalinbox.service.LinkStatus
import app.portalinbox.ui.Ages
import app.portalinbox.ui.VisibleText
import java.time.ZoneId

/** What the home list says about the link to one portal. */
enum class PortalHealth {
    /** Connected, or not connected for less than the offline threshold of the portal. */
    OK,

    /** Not connected for longer than the offline threshold. Answers wait in the outbox. */
    OFFLINE,

    /** The portal does not prove the pinned key. The phone sends nothing to it. */
    KEY_CHANGED,

    /** The portal revoked this device, or the address is invalid. The phone sends nothing to it. */
    RE_ENROLL;

    /** The rows of the portal are dimmed, its chip is dashed and its dot is hollow. */
    val isDown: Boolean get() = this != OK

    /** The problem needs an action of the user. */
    val isDanger: Boolean get() = this == KEY_CHANGED || this == RE_ENROLL
}

/** One portal as the home list draws it: the identity and the state of the link. */
data class HomePortal(
    val identity: PortalIdentity,
    val health: PortalHealth,
    /** The "New" mark: the portal was added in the last 24 hours. */
    val isNew: Boolean,
    /** Since when the portal is not reachable. Null when [health] is not [PortalHealth.OFFLINE]. */
    val offlineSinceMs: Long?,
    /** The time of the last frame of the portal, on the phone clock. Null = never connected. */
    val lastSyncMs: Long?,
) {
    val id: Long get() = identity.id

    companion object {
        /**
         * The portals of the home list, in the order of the user. [link] gives the state of the link of a portal. The offline
         * threshold is the `offlineWarningMs` of the portal, the same one that the service uses for its warning, so the list
         * and the warning agree.
         */
        fun build(servers: List<Server>, link: (Long) -> LinkStatus, nowMs: Long): List<HomePortal> =
            servers.sortedWith(compareBy<Server> { it.sortIndex }.thenBy { it.id }).map { s ->
                val st = link(s.id)
                val health = OfflineSummary.health(st, s.offlineWarningMs, nowMs)
                HomePortal(
                    identity = PortalIdentity.of(s),
                    health = health,
                    isNew = PortalIdentity.isNew(s.addedAtMs, nowMs),
                    offlineSinceMs = if (health == PortalHealth.OFFLINE) s.lastSyncMs?.takeIf { it <= st.sinceMs } ?: st.sinceMs else null,
                    lastSyncMs = s.lastSyncMs,
                )
            }
    }
}

/**
 * The warnings of the home list for the portals that cannot be reached. Pure Kotlin.
 *
 * These are banners inside the screen. They are not notifications: the one notification of the app is a separate part.
 */
object OfflineSummary {
    /** The most banners at the same time. More problems fold into one line that opens the list of portals. */
    const val MAX_BANNERS = 3

    enum class Kind { NO_NETWORK, OFFLINE, KEY_CHANGED, RE_ENROLL, MORE }

    enum class Severity { WARNING, DANGER }

    data class Banner(
        val kind: Kind,
        val severity: Severity,
        /** The portal of the banner. Null for the banners that name no portal. */
        val portalId: Long?,
        val title: String,
        val text: String,
        /** The text of the action button: "Fix" opens the portal. Null = no button. */
        val action: String?,
    )

    /** The state of the link of a portal. [warnAfterMs] is its offline threshold. */
    fun health(link: LinkStatus, warnAfterMs: Long, nowMs: Long): PortalHealth = when (link.state) {
        LinkState.CONNECTED -> PortalHealth.OK
        LinkState.KEY_CHANGED -> PortalHealth.KEY_CHANGED
        LinkState.REENROLL -> PortalHealth.RE_ENROLL
        LinkState.CONNECTING, LinkState.DISCONNECTED ->
            if (nowMs - link.sinceMs >= warnAfterMs) PortalHealth.OFFLINE else PortalHealth.OK
    }

    /**
     * The banners, the danger ones first. Without a network the phone shows one banner for the whole phone and no banner for
     * a portal that is only offline. A portal with a key problem keeps its own banner: only the user can fix it.
     */
    fun banners(portals: List<HomePortal>, networkAvailable: Boolean, nowMs: Long, zone: ZoneId): List<Banner> {
        val danger = portals.filter { it.health.isDanger }.map { dangerBanner(it) }
        val noNetwork = if (!networkAvailable && portals.isNotEmpty()) {
            listOf(Banner(Kind.NO_NETWORK, Severity.WARNING, null, "No network.", "All portals are paused. Your answers wait on the phone.", null))
        } else {
            emptyList()
        }
        val offline = if (networkAvailable) portals.filter { it.health == PortalHealth.OFFLINE }.map { offlineBanner(it, nowMs, zone) } else emptyList()
        val all = danger + noNetwork + offline
        if (all.size <= MAX_BANNERS) return all
        val shown = all.take(MAX_BANNERS - 1)
        val rest = all.size - shown.size
        val more = Banner(Kind.MORE, Severity.WARNING, null, "$rest more portals have a problem.", "Open the list of portals to see them.", "Show")
        return shown + more
    }

    private fun name(p: HomePortal) = VisibleText.escapeLine(p.identity.nickname)

    private fun dangerBanner(p: HomePortal): Banner =
        if (p.health == PortalHealth.KEY_CHANGED) {
            Banner(Kind.KEY_CHANGED, Severity.DANGER, p.id, "${name(p)}: key check failed.", "Not connected. Nothing is sent to it.", "Fix")
        } else {
            Banner(Kind.RE_ENROLL, Severity.DANGER, p.id, "${name(p)}: enroll this phone again.", "Not connected. Nothing is sent to it.", "Fix")
        }

    private fun offlineBanner(p: HomePortal, nowMs: Long, zone: ZoneId): Banner {
        val since = p.offlineSinceMs ?: nowMs
        return Banner(
            Kind.OFFLINE, Severity.WARNING, p.id,
            "${name(p)} is offline since ${Ages.stamp(since, nowMs, zone)} (${Ages.short(since, nowMs)}).",
            "Its requests may be out of date. Your answers wait on the phone.", null,
        )
    }

    /**
     * The last line of a row of a portal that is down, in place of the usual ages. Null when the portal is fine. An offline
     * portal can still be answered: the answer waits in the outbox. A portal with a key problem cannot receive anything.
     */
    fun rowReason(p: HomePortal, nowMs: Long): String? = when (p.health) {
        PortalHealth.OK -> null
        PortalHealth.OFFLINE -> "Offline ${Ages.short(p.offlineSinceMs ?: nowMs, nowMs)} · answer waits"
        PortalHealth.KEY_CHANGED, PortalHealth.RE_ENROLL ->
            "Not connected · " + (p.lastSyncMs?.let { "last sync ${Ages.ago(it, nowMs)}" } ?: "never synced")
    }
}
