package app.portalinbox.alert

import app.portalinbox.data.Alert
import app.portalinbox.data.Item
import app.portalinbox.data.Levels
import app.portalinbox.portal.PortalIdentity
import app.portalinbox.proto.QuietPrefs
import app.portalinbox.proto.RequestKind
import app.portalinbox.proto.Urgency
import app.portalinbox.service.LinkState

/**
 * The one notification of the phone (decision "one notification", lot MP4). Pure Kotlin: no Context.
 *
 * The phone shows at most ONE notification for everything that waits: approvals, questions, replies of agents, alerts and the
 * warnings of portals that are down. It covers all portals. It has one fixed id and one fixed tag, and it is updated in place.
 * [compute] reads the state of the repositories and gives the [Aggregate]: counts for each portal, the oldest age, the most
 * urgent thing, the alarm and the single approval. The digest of a portal is one more entry and one more part of its line
 * ([DigestPlan], lot MP5): it never makes a second notification and it never makes a sound. [decide] says if the notification must alert again. [plan] says what to post.
 * The texts are in [AttentionText]. [AttentionCenter] runs the three steps. The Android object [AttentionNotifier] only draws.
 */
object Attention {
    /** The one id and the one tag of the notification of items and alerts. No other notification of the app uses them. */
    const val ID = 3000
    const val TAG = "attention"

    /** The channel of a notification that can alert. It has no sound of its own: the code plays the sound that fits. */
    const val CH_ATTENTION = "attention"

    /** The channel of an update with no sound and no heads-up. Android cannot silence one notification below API 34, so the code picks the channel. */
    const val CH_QUIET = "attention_quiet"

    /** How urgent a thing is. The order is the order of the values. */
    enum class Rank { LOW, NORMAL, HIGH, ALARM }

    /** The sound of a new thing. [PHONE]: the sound mode of the phone decides. [RING]: one sound on the alarm stream. [ALARM]: the looping alarm. */
    enum class Sound { NONE, PHONE, RING, ALARM }

    enum class What { APPROVAL, QUESTION, REPLY, ALERT, DOWN, DIGEST }

    enum class DownKind { OFFLINE, REENROLL, KEY_CHANGED }

    /** A portal that does not connect and that deserves a warning. [sinceMs] is the time of the phone. */
    data class Down(val kind: DownKind, val sinceMs: Long)

    /** What the aggregate needs to know about one portal. [nowMs] is the time of that portal (the clock of its server). */
    data class Portal(
        val identity: PortalIdentity,
        val hideNameOnLock: Boolean,
        /** The user muted this portal: its items and alerts make no sound. An alarm still rings. */
        val muted: Boolean,
        val quiet: QuietPrefs?,
        val nowMs: Long,
        val down: Down? = null,
    )

    /** An unread turn of an agent in a discussion that is still open. */
    data class Reply(val serverId: Long, val requestId: String, val urgency: Urgency)

    /**
     * [items]: the items that still wait for the owner (open, with no answer on its way). [alerts]: the unread alerts.
     * [phoneNowMs]: the time of the phone, for the quiet hours and the age of a warning. [network]: false when the phone has no
     * network. Then no portal warning is made: the status notification says "No network" once. [digests]: what still waits
     * at each portal that sent a current digest ([DigestPlan.merge]).
     */
    data class Input(
        val portals: List<Portal>,
        val items: List<Item>,
        val alerts: List<Alert>,
        val replies: List<Reply> = emptyList(),
        val phoneNowMs: Long = 0L,
        val network: Boolean = true,
        val digests: List<DigestPlan.Waiting> = emptyList(),
    )

    /** One thing in the notification. [key] tells which one. [seen]: the phone already showed it before the process started. */
    data class Entry(val key: String, val serverId: Long, val what: What, val rank: Rank, val sound: Sound, val seen: Boolean)

    /** What the notification says about one portal. */
    data class Line(
        val identity: PortalIdentity,
        val hideNameOnLock: Boolean,
        val approvals: Int,
        val questions: Int,
        val replies: Int,
        val alerts: Int,
        val down: Down?,
        val oldestAgeMs: Long?,
        /** The time of the phone for the age of [down]. */
        val phoneNowMs: Long,
        /** What the digest of this portal says still waits. 0: no digest part. */
        val waiting: Long = 0,
        /** The age of the oldest waiting item of the digest, on the clock of the portal. */
        val waitingAgeMs: Long? = null,
    ) {
        val toAnswer: Int get() = approvals + questions
        val isEmpty: Boolean get() = toAnswer == 0 && replies == 0 && alerts == 0 && down == null && waiting == 0L
    }

    data class Aggregate(
        /** The portals that have something to say, in the order of the user. */
        val lines: List<Line>,
        val entries: List<Entry>,
        val approvals: Int,
        val questions: Int,
        val replies: Int,
        val alerts: Int,
        val down: Int,
        /** What the digests say still waits, all portals together. */
        val waiting: Long,
        /** The age of the oldest item, alert or waiting digest item, on the clock of its portal. Null when there is none. */
        val oldestAgeMs: Long?,
        val maxRank: Rank?,
        /** The alarms that ring now (unread, not stopped). */
        val ringing: List<Alert>,
        /** The only approval when the notification holds exactly one approval and nothing else. */
        val singleApproval: Item?,
    ) {
        val toAnswer: Int get() = approvals + questions
        val isEmpty: Boolean get() = lines.isEmpty()

        /** The shape of the notification without the ages. A refresh that does not change it is not posted again by a tick. */
        val key: String
            get() = entries.sortedBy { it.key }.joinToString(",") { "${it.key}:${it.rank}:${it.sound}" } +
                "|" + lines.joinToString(",") { "${it.identity.id}:${it.identity.nickname}:${it.hideNameOnLock}:${it.down?.kind}" } +
                "|" + ringing.joinToString(",") { "${it.serverId}:${it.alertId}" }
    }

    fun rankOf(urgency: Urgency): Rank = when (urgency) {
        Urgency.LOW -> Rank.LOW
        Urgency.NORMAL -> Rank.NORMAL
        Urgency.HIGH -> Rank.HIGH
    }

    /**
     * The warning that a portal deserves, or null. A portal that needs the user (re-enroll, changed key) warns at once. A
     * portal that is only offline warns after [offlineWarningMs]. A portal that connects or tries to connect does not warn.
     */
    fun downOf(state: LinkState, sinceMs: Long, offlineWarningMs: Long, nowMs: Long): Down? = when (state) {
        LinkState.CONNECTED, LinkState.CONNECTING -> null
        LinkState.REENROLL -> Down(DownKind.REENROLL, sinceMs)
        LinkState.KEY_CHANGED -> Down(DownKind.KEY_CHANGED, sinceMs)
        LinkState.DISCONNECTED -> if (nowMs - sinceMs >= offlineWarningMs) Down(DownKind.OFFLINE, sinceMs) else null
    }

    /** Computes the aggregate. A thing of a portal that is not in [Input.portals] (a portal that was removed) does not count. */
    fun compute(input: Input): Aggregate {
        val entries = LinkedHashMap<String, Entry>()
        val lines = ArrayList<Line>()
        val ringing = ArrayList<Alert>()
        var singleCandidate: Item? = null
        var approvals = 0
        var questions = 0
        var replies = 0
        var alerts = 0
        var down = 0
        var waiting = 0L
        var oldest: Long? = null
        for (p in input.portals) {
            val id = p.identity.id
            val open = input.items.filter { it.serverId == id && it.isOpen }
            val unread = input.alerts.filter { it.serverId == id && it.unread && it.action != Levels.OFF }
            val pReplies = input.replies.filter { it.serverId == id && open.any { i -> i.requestId == it.requestId } }
            var age: Long? = null
            fun older(createdAtMs: Long) {
                val a = (p.nowMs - createdAtMs).coerceAtLeast(0)
                if (age == null || a > age!!) age = a
            }
            for (i in open) {
                val r = i.request
                val silent = p.muted || QuietHours.silent(r.quiet, r.urgency, p.quiet, input.phoneNowMs)
                val rank = rankOf(r.urgency)
                val what = if (i.kind == RequestKind.APPROVAL) What.APPROVAL else What.QUESTION
                val key = "i:$id:${r.chainId}"
                entries.putIfAbsent(key, Entry(key, id, what, rank, if (silent) Sound.NONE else Sound.RING, i.notified))
                older(r.createdAtMs)
                if (what == What.APPROVAL) {
                    approvals++
                    singleCandidate = i
                } else {
                    questions++
                }
            }
            for (rep in pReplies) {
                val item = open.first { it.requestId == rep.requestId }
                val silent = p.muted || QuietHours.silent(false, rep.urgency, p.quiet, input.phoneNowMs)
                val key = "r:$id:${rep.requestId}"
                entries.putIfAbsent(key, Entry(key, id, What.REPLY, rankOf(item.request.urgency), if (silent) Sound.NONE else Sound.PHONE, false))
            }
            replies += pReplies.size
            for (a in unread) {
                val (rank, sound) = when (a.action) {
                    Levels.ALARM -> Rank.ALARM to Sound.ALARM
                    Levels.RING -> Rank.HIGH to (if (p.muted) Sound.NONE else Sound.RING)
                    else -> Rank.NORMAL to (if (p.muted) Sound.NONE else Sound.PHONE)
                }
                val key = "a:$id:${a.alertId}"
                entries.putIfAbsent(key, Entry(key, id, What.ALERT, rank, sound, a.notified))
                older(a.createdAtMs)
                if (a.action == Levels.ALARM && a.stoppedAtMs == null) ringing.add(a)
            }
            alerts += unread.size
            val dn = if (input.network) p.down else null
            if (dn != null) {
                val key = "d:$id:${dn.kind}"
                entries.putIfAbsent(key, Entry(key, id, What.DOWN, Rank.NORMAL, if (p.muted) Sound.NONE else Sound.PHONE, false))
                down++
            }
            // The digest of the portal: a silent entry (a new digest brings the notification back without a sound) and a part of its line.
            val digest = input.digests.firstOrNull { it.serverId == id }
            var waitingAge: Long? = null
            if (digest != null) {
                val key = "g:$id:${digest.deliveryId}"
                entries.putIfAbsent(key, Entry(key, id, What.DIGEST, Rank.LOW, Sound.NONE, false))
                older(digest.oldestCreatedAtMs)
                waitingAge = (p.nowMs - digest.oldestCreatedAtMs).coerceAtLeast(0)
                waiting += digest.count
            }
            val line = Line(
                identity = p.identity, hideNameOnLock = p.hideNameOnLock, approvals = open.count { it.kind == RequestKind.APPROVAL },
                questions = open.count { it.kind != RequestKind.APPROVAL }, replies = pReplies.size, alerts = unread.size, down = dn,
                oldestAgeMs = age, phoneNowMs = input.phoneNowMs, waiting = digest?.count ?: 0, waitingAgeMs = waitingAge,
            )
            if (!line.isEmpty) lines.add(line)
            if (age != null && (oldest == null || age!! > oldest!!)) oldest = age
        }
        // A digest that says more than this one approval waits is more than one approval: no action then.
        val single = if (approvals == 1 && questions == 0 && replies == 0 && alerts == 0 && down == 0 && waiting <= 1) singleCandidate else null
        return Aggregate(
            lines = lines, entries = entries.values.toList(), approvals = approvals, questions = questions, replies = replies,
            alerts = alerts, down = down, waiting = waiting, oldestAgeMs = oldest, maxRank = entries.values.maxOfOrNull { it.rank },
            ringing = ringing, singleApproval = single,
        )
    }

    /**
     * Whether the notification alerts again, and with which sound.
     *
     * The phone alerts again only for a NEW thing that can make a sound and that is at least as urgent as what the notification
     * already shows (the things that stay). A new low item, a new thing in the quiet window, and a new normal thing under a
     * high one update the text without sound. [shown] is what the notification showed at its last update (key and rank). Null:
     * the process just started, and the things that the phone already notified (`seen`) count as shown.
     */
    fun decide(shown: Map<String, Rank>?, a: Aggregate): Decision {
        val before: Map<String, Rank> = shown ?: a.entries.filter { it.seen }.associate { it.key to it.rank }
        val fresh = a.entries.filter { it.key !in before }
        val stay = a.entries.filter { it.key in before }
        val floor = stay.maxOfOrNull { it.rank.ordinal } ?: -1
        val loud = fresh.filter { it.sound != Sound.NONE && it.rank.ordinal >= floor }
        val sound = loud.maxOfOrNull { it.sound } ?: Sound.NONE
        return Decision(alert = loud.isNotEmpty(), sound = sound, arrivals = fresh.isNotEmpty())
    }

    /** [arrivals]: something new is in the notification, with or without sound. */
    data class Decision(val alert: Boolean, val sound: Sound, val arrivals: Boolean)

    /** The screen that a tap opens: the home list, or the alarm page while an alarm rings. */
    sealed interface Target {
        data object Home : Target
        data class Alarm(val serverId: Long, val alertId: String) : Target
    }

    data class Plan(
        val content: AttentionText.Content,
        val channel: String,
        /** False: the notification updates with no sound and no new heads-up. */
        val alert: Boolean,
        val sound: Sound,
        val target: Target,
        /** The action Decline: only when the notification holds exactly one approval and nothing else. Never Approve. */
        val decline: Pair<Long, String>?,
        /** The action "Stop alarm": an alarm rings. */
        val stopAlarm: Boolean,
        /** An alarm rings: category alarm, ongoing, red. */
        val alarm: Boolean,
        /** The full-screen intent of the alarm page. Off when the page is already on screen. */
        val fullScreen: Boolean,
        /** The color index of the portal when the notification is about one portal only. */
        val colorIndex: Int?,
    ) {
        val id: Int get() = ID
        val tag: String get() = TAG
    }

    /**
     * What to post. [alarmDirect]: the alarm page is on screen or opens directly, so no heads-up and no full-screen intent.
     * [shown]: the notification is on screen now. A notification that is not there starts quiet unless it alerts.
     */
    fun plan(a: Aggregate, d: Decision, alarmDirect: Boolean, shown: Boolean): Plan {
        val ringing = a.ringing.isNotEmpty()
        val direct = ringing && alarmDirect
        val loudCapable = a.entries.any { it.sound != Sound.NONE }
        val channel = when {
            direct -> CH_QUIET
            ringing -> CH_ATTENTION
            d.alert || (shown && loudCapable) -> CH_ATTENTION
            else -> CH_QUIET
        }
        val first = a.ringing.firstOrNull()
        return Plan(
            content = AttentionText.of(a),
            channel = channel,
            alert = d.alert,
            sound = d.sound,
            target = if (first != null) Target.Alarm(first.serverId, first.alertId) else Target.Home,
            decline = a.singleApproval?.let { it.serverId to it.requestId },
            stopAlarm = ringing,
            alarm = ringing,
            fullScreen = ringing && !direct,
            colorIndex = a.lines.singleOrNull()?.identity?.colorIndex,
        )
    }
}
