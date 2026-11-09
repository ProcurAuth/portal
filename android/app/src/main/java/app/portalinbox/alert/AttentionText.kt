package app.portalinbox.alert

import app.portalinbox.ui.Ages
import app.portalinbox.ui.VisibleText

/**
 * The texts of the one notification. Pure Kotlin: no Context.
 *
 * Two versions exist. The private version shows on an unlocked phone: the counts, the nickname of each portal, and the age of
 * the oldest thing. The public version shows on a locked screen: the same counts, and the nickname of a portal only when the user
 * did not hide it for that portal. Neither version holds a command, a body, a title of a request, an agent text or an
 * explanation. A nickname goes through [VisibleText.escapeLine] anyway.
 */
object AttentionText {
    /**
     * [title] and [text]: the two lines of the notification. [lines]: one line for each portal, for the expanded view. It is empty
     * when one portal holds everything and it is up. [publicTitle] and [publicText]: the version for the lock screen.
     */
    data class Content(val title: String, val text: String, val lines: List<String>, val publicTitle: String, val publicText: String)

    const val PUBLIC_HINT = "Unlock to see them."

    private fun name(p: Attention.Line) = VisibleText.escapeLine(p.identity.nickname)

    private fun count(n: Int, one: String, many: String) = if (n == 1) "1 $one" else "$n $many"

    /** "3 to answer, 1 reply, 2 alerts". Empty when none of the three counts has a value. */
    fun counts(toAnswer: Int, replies: Int, alerts: Int): String = listOfNotNull(
        if (toAnswer > 0) "$toAnswer to answer" else null,
        if (replies > 0) count(replies, "reply", "replies") else null,
        if (alerts > 0) count(alerts, "alert", "alerts") else null,
    ).joinToString(", ")

    /** The counts of one portal: "2 to answer, 1 alert". */
    private fun countsOf(p: Attention.Line) = counts(p.toAnswer, p.replies, p.alerts)

    fun downText(p: Attention.Line): String? {
        val d = p.down ?: return null
        return when (d.kind) {
            Attention.DownKind.OFFLINE -> "not connected for " + Ages.long((p.phoneNowMs - d.sinceMs).coerceAtLeast(0))
            Attention.DownKind.REENROLL -> "re-enrollment needed"
            Attention.DownKind.KEY_CHANGED -> "key changed, not connected"
        }
    }

    /** "Production: 2 to answer, 1 alert" or "Homelab: not connected for 3 hours" or "Homelab: 1 to answer · not connected for 3 hours". */
    fun lineOf(p: Attention.Line): String {
        val parts = listOfNotNull(countsOf(p).ifEmpty { null }, downText(p))
        return name(p) + ": " + parts.joinToString(" · ")
    }

    fun of(a: Attention.Aggregate): Content {
        val counts = counts(a.toAnswer, a.replies, a.alerts)
        val names = a.lines.joinToString(", ") { name(it) }
        val oldest = a.oldestAgeMs?.let { "the oldest waits " + Ages.long(it) }
        val title: String
        val publicTitle: String
        if (counts.isNotEmpty()) {
            title = counts
            publicTitle = counts
        } else {
            // Only warnings of portals that are down.
            val down = a.lines.filter { it.down != null }
            title = if (down.size == 1) name(down[0]) + " is not connected" else "${down.size} portals are not connected"
            publicTitle = if (down.size == 1 && !down[0].hideNameOnLock) title else if (down.size == 1) "A portal is not connected" else title
        }
        val downNames = if (counts.isEmpty()) "" else a.lines.filter { it.down != null }.let { d ->
            if (d.isEmpty()) "" else d.joinToString(", ") { name(it) } + (if (d.size == 1) " is" else " are") + " not connected"
        }
        val text = if (counts.isEmpty()) {
            val d = a.lines.first { it.down != null }
            if (a.lines.size == 1) downText(d)!!.replaceFirstChar { it.uppercase() } + ". Its requests wait." else "The phone cannot reach them. Their requests wait."
        } else {
            listOfNotNull(names, oldest?.replaceFirstChar { it.uppercase() }, downNames.ifEmpty { null }).joinToString(" · ")
        }
        // One portal that is up: the two lines say it all. Else one line for each portal.
        val lines = if (a.lines.size == 1 && a.lines[0].down == null) emptyList() else a.lines.map { lineOf(it) }
        val shown = a.lines.filter { !it.hideNameOnLock }.joinToString(", ") { name(it) }
        val publicText = if (shown.isEmpty()) PUBLIC_HINT else "$shown · $PUBLIC_HINT"
        return Content(title, text, lines, publicTitle, publicText)
    }
}
