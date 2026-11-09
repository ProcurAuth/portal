package app.portalinbox.portal

import app.portalinbox.data.PendingPrefs
import app.portalinbox.data.PrefsSync
import app.portalinbox.proto.DigestMode
import app.portalinbox.proto.Prefs
import app.portalinbox.proto.SetPrefsFrame
import app.portalinbox.service.LinkState

/**
 * "Apply quiet hours and summary to all portals" (proposal 3.10, answer Q5). Pure Kotlin.
 *
 * `set_prefs` replaces the preferences of the account at one portal, so the phone sends it to each portal on its own. The sheet
 * of the settings screen shows one line for each other portal, and says what will happen to it before the user decides:
 * - [Action.SEND]: the portal is connected. The frame goes out now.
 * - [Action.QUEUE]: the portal is not connected now. The phone keeps the values as the pending edit of that portal and sends
 *   them at its next `welcome` ([PrefsSync]).
 * - [Action.SKIP]: nothing can be sent: the key changed, the device was revoked, the portal has no requests, or it has these values.
 *
 * [plan] only reads. [run] writes, and only for the portals that the user chose. Nothing here runs by itself.
 */
object PrefsApply {
    enum class Action { SEND, QUEUE, SKIP }

    enum class Reason { NOW, WHEN_BACK, NOT_CONNECTED, ITEMS_OFF, SAME }

    /** What the sheet needs to know about one portal. */
    data class Target(
        val identity: PortalIdentity,
        val state: LinkState,
        /** The portal handles requests, so it takes `set_prefs`. False: its last `welcome` had no purge age, or it never connected. */
        val itemsEnabled: Boolean,
        /** The login of the account at that portal. */
        val account: String,
        /** The preferences that apply at that portal now: the pending edit, else its last `welcome`. Null: unknown. */
        val effective: Prefs?,
    )

    /** One line of the sheet. */
    data class Step(val target: Target, val action: Action, val reason: Reason, val accountDiffers: Boolean) {
        /** The user can choose this line. A line that cannot send is shown and cannot be ticked. */
        val selectable: Boolean get() = action != Action.SKIP

        /** The line starts ticked when it can send. */
        val ticked: Boolean get() = selectable

        /** The text under the name. */
        val text: String
            get() = when (reason) {
                Reason.NOW -> "Sent now"
                Reason.WHEN_BACK -> "Offline. Sent when it is back."
                Reason.NOT_CONNECTED -> "Not connected. Not sent."
                Reason.ITEMS_OFF -> "No requests on this portal. Not sent."
                Reason.SAME -> "Already has these values."
            }

        /** The line about the account, when the login there is not the login of the portal that the user is on. */
        val accountText: String? get() = if (accountDiffers) "Account @${target.account}. Its values are replaced." else null
    }

    /**
     * The lines of the sheet for [targets] (the other portals) and the [values] to apply. [sourceAccount] is the login at the
     * portal that the user is on: a line with another login says so, because the values replace the ones of that account.
     */
    fun plan(sourceAccount: String, targets: List<Target>, values: Prefs): List<Step> = targets.map { t ->
        val differs = t.account != sourceAccount
        val (action, reason) = when {
            t.state == LinkState.KEY_CHANGED || t.state == LinkState.REENROLL -> Action.SKIP to Reason.NOT_CONNECTED
            !t.itemsEnabled -> Action.SKIP to Reason.ITEMS_OFF
            t.effective == values -> Action.SKIP to Reason.SAME
            t.state == LinkState.CONNECTED -> Action.SEND to Reason.NOW
            else -> Action.QUEUE to Reason.WHEN_BACK
        }
        Step(t, action, reason, differs)
    }

    /** What [run] did. The ids are the ids of the portals. */
    data class Outcome(val sent: List<Long>, val queued: List<Long>)

    /**
     * Applies [values] to the portals in [chosen] that can take them. A chosen portal with the action SKIP is left alone. Each
     * portal gets its pending edit ([PrefsSync.edit]) and its frame: [send] is called for each one, and a portal that is not
     * connected then keeps the pending edit until its next `welcome`. A portal that the user did not choose is never touched.
     */
    fun run(steps: List<Step>, chosen: Set<Long>, values: Prefs, pending: PendingPrefs, send: (Long, SetPrefsFrame) -> Unit): Outcome {
        val sent = ArrayList<Long>()
        val queued = ArrayList<Long>()
        for (step in steps) {
            val id = step.target.identity.id
            if (id !in chosen || step.action == Action.SKIP) continue
            send(id, PrefsSync.edit(pending, id, values))
            if (step.action == Action.SEND) sent.add(id) else queued.add(id)
        }
        return Outcome(sent, queued)
    }

    /** The label of the button: "Apply to 2 portals". */
    fun buttonText(count: Int): String = if (count == 1) "Apply to 1 portal" else "Apply to $count portals"

    /** The line after the apply: "Sent to 1 portal. 1 portal gets it when it is back." Null when nothing ran. */
    fun resultText(o: Outcome): String? {
        fun portals(n: Int) = if (n == 1) "1 portal" else "$n portals"
        val parts = listOfNotNull(
            if (o.sent.isNotEmpty()) "Sent to ${portals(o.sent.size)}." else null,
            if (o.queued.isNotEmpty()) "${portals(o.queued.size)} get${if (o.queued.size == 1) "s" else ""} it when back online." else null,
        )
        return parts.joinToString(" ").ifEmpty { null }
    }

    /**
     * What the sheet says about the values: "Quiet hours 22:00 to 07:00 (Europe/Paris) and a daily summary at 09:00. Each portal
     * keeps its own account." The time zone is the one that is sent.
     */
    fun describe(values: Prefs): String {
        val q = values.quiet
        val quiet = if (q == null) "No quiet hours" else "Quiet hours ${q.from} to ${q.to} (${q.tz})"
        val d = values.digest
        val digest = when (d?.mode) {
            null -> "the default summary of each portal"
            DigestMode.OFF -> "no summary"
            DigestMode.DAILY -> "a daily summary at ${d.at}" + if (q == null || q.tz != d.tz) " (${d.tz})" else ""
            DigestMode.EVERY -> "a summary every ${d.everyH} h"
        }
        val rings = if (q != null && q.highRings) " Urgent items still ring." else ""
        return "$quiet and $digest.$rings Each portal keeps its own account."
    }
}
