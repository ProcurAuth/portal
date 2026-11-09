package app.portalinbox.alert

import app.portalinbox.proto.QuietPrefs
import app.portalinbox.proto.Urgency
import java.time.Instant
import java.time.ZoneId

/**
 * The quiet hours of the account (DIAL 3). Pure Kotlin.
 *
 * The server never holds an item back: it sets `quiet` on a request that came inside the window. The phone also checks the
 * window with its own clock, so a notification that the phone shows later (a reconnection after the window began, a
 * message of an agent) stays silent too. The window is the interval `from <= time < to` in the time zone of the prefs. It
 * can cross midnight. An item of urgency `high` still rings when `high_rings` is true.
 */
object QuietHours {
    /** The minutes since midnight of "HH:MM". The text was checked when the prefs were read. */
    fun minutes(hhmm: String): Int = hhmm.substring(0, 2).toInt() * 60 + hhmm.substring(3, 5).toInt()

    private fun zoneOf(tz: String): ZoneId? = runCatching { ZoneId.of(tz) }.getOrNull()

    /** True when [nowMs] is inside the window. A time zone that this phone does not know gives false: a sound is never lost for it. */
    fun active(q: QuietPrefs, nowMs: Long): Boolean {
        val zone = zoneOf(q.tz) ?: return false
        val t = Instant.ofEpochMilli(nowMs).atZone(zone).toLocalTime()
        val now = t.hour * 60 + t.minute
        val from = minutes(q.from)
        val to = minutes(q.to)
        return when {
            from == to -> false
            from < to -> now in from until to
            else -> now >= from || now < to
        }
    }

    /** True when a notification of this urgency must not make a sound now. */
    fun suppressesSound(q: QuietPrefs?, urgency: Urgency, nowMs: Long): Boolean {
        if (q == null || !active(q, nowMs)) return false
        return !(urgency == Urgency.HIGH && q.highRings)
    }

    /**
     * Whether a notification is silent. A request that the server marked `quiet`, and a low urgency, are silent. Inside the
     * window the phone adds its own check. An item of urgency `high` rings when the prefs say `high_rings`, even if the
     * server marked it `quiet`: the owner asked for it.
     */
    fun silent(frameQuiet: Boolean, urgency: Urgency, q: QuietPrefs?, nowMs: Long): Boolean {
        if (urgency == Urgency.LOW) return true
        val highRings = urgency == Urgency.HIGH && q?.highRings == true
        if (highRings) return false
        return frameQuiet || (q != null && active(q, nowMs))
    }

    /** "22:00 to 07:00". */
    fun describe(q: QuietPrefs): String = "${q.from} to ${q.to}"
}
