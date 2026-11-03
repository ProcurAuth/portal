package app.portalinbox.data

/** What the session must send so the server's subscriptions match the local levels. */
data class Reconcile(val subscribe: List<String>, val unsubscribe: List<String>)

/** The local levels to store after a Welcome, and what to send to the server. */
data class LevelPlan(val levels: Map<String, String>, val reconcile: Reconcile)

/**
 * The local level of a group. The server never knows it: the phone keeps it and applies it.
 *
 * The id [DIRECT_ID] is a pseudo group. The server sends an alert for one account as a normal
 * alert frame with this id. The group is not in the configuration of the server. The phone
 * never subscribes to it and never unsubscribes from it. Its default level is `ring`, never
 * `alarm`. The user can change this level, or set it off, in the settings of the server.
 */
object Levels {
    const val OFF = "off"
    const val NOTIFY = "notify"
    const val RING = "ring"
    const val ALARM = "alarm"

    /** The group id that the server sends for an alert to one account. */
    const val DIRECT_ID = "@direct"

    /** The group name that the server sends for an alert to one account. */
    const val DIRECT_NAME = "Direct"

    /** The level of the pseudo group [DIRECT_ID] when the user has not chosen one. */
    const val DIRECT_DEFAULT = RING

    /** The level of a group of the server when the user has not chosen one. */
    const val GROUP_DEFAULT = NOTIFY

    private val ACTIVE = setOf(ALARM, RING, NOTIFY)

    fun isDirect(group: String): Boolean = group == DIRECT_ID

    /**
     * The groups for the settings screen: the groups of the server, then the pseudo group
     * [DIRECT_ID]. The pseudo group is always there. A group of the server with this id is
     * dropped.
     */
    fun withDirect(groups: List<Group>, description: String): List<Group> =
        groups.filterNot { isDirect(it.id) } + Group(DIRECT_ID, DIRECT_NAME, description)

    /**
     * The level to show for a group: `alarm`, `ring` or `notify`, or null for off.
     * A group of the server has no level until the user follows it, so it is off.
     */
    fun shown(group: String, levels: Map<String, String>): String? {
        val level = levels[group]
        return when {
            level in ACTIVE -> level
            level == OFF -> null
            isDirect(group) -> DIRECT_DEFAULT
            else -> null
        }
    }

    /**
     * The action stored with an alert that arrives for this group. An unknown group gets
     * [GROUP_DEFAULT]. The pseudo group gets [DIRECT_DEFAULT]. An alert of a group that the
     * user turned off is still shown as a notification, because the server already chose it.
     * For the pseudo group, off is final: the alert is stored with the action `off`, without
     * sound and without notification.
     */
    fun alertAction(group: String, levels: Map<String, String>): String {
        val level = levels[group]
        if (level in ACTIVE) return level!!
        return when {
            isDirect(group) -> if (level == OFF) OFF else DIRECT_DEFAULT
            else -> GROUP_DEFAULT
        }
    }

    /** True when a change of the level of this group must go to the server. */
    fun isServerGroup(group: String): Boolean = !isDirect(group)

    /**
     * Reconciles the local levels with the Welcome. A local level of a group that the server
     * does not have is dropped, except the level of the pseudo group. A subscribed group
     * without a local level gets `notify`. The plan never names the pseudo group, whatever
     * the server lists.
     */
    fun reconcile(local: Map<String, String>, groupIds: Set<String>, serverSubscriptions: Set<String>): LevelPlan {
        val known = groupIds.filterNot { isDirect(it) }.toSet()
        val merged = local.filterKeys { it in known || isDirect(it) }.toMutableMap()
        for (g in serverSubscriptions) if (g in known && g !in merged) merged[g] = NOTIFY
        val onServer = merged.filterKeys { isServerGroup(it) }
        return LevelPlan(
            levels = merged,
            reconcile = Reconcile(
                subscribe = onServer.filter { it.value != OFF && it.key !in serverSubscriptions }.keys.sorted(),
                unsubscribe = onServer.filter { it.value == OFF && it.key in serverSubscriptions }.keys.sorted(),
            ),
        )
    }

    /**
     * The groups that a `subscriptions` message from the server turns off on the phone: groups
     * that are on locally, are not on the server, and were not asked for a moment ago. The
     * pseudo group is never in the list.
     */
    fun lostOnServer(
        local: Map<String, String>, server: Set<String>, askedAtMs: Map<String, Long>, nowMs: Long, settleMs: Long,
    ): List<String> = local.filter { (group, level) ->
        isServerGroup(group) && level != OFF && group !in server &&
            askedAtMs[group].let { it == null || nowMs - it >= settleMs }
    }.keys.sorted()
}
