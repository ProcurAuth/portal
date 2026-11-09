package app.portalinbox.ui

import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale

/**
 * The words for ages and times on the item screens. Pure Kotlin: English only, no Context. The age of an item
 * counts from the clock of its server (`ClockOffsets`), so a phone with a wrong clock shows the right age.
 */
object Ages {
    /** "just now", "5 min ago", "3 h ago", "yesterday", "6 d ago". */
    fun ago(thenMs: Long, nowMs: Long): String {
        val s = ((nowMs - thenMs) / 1000).coerceAtLeast(0)
        return when {
            s < 45 -> "just now"
            s < 3_600 -> "${(s + 30) / 60} min ago"
            s < 86_400 -> "${s / 3_600} h ago"
            s < 172_800 -> "yesterday"
            else -> "${s / 86_400} d ago"
        }
    }

    /** "<1 min", "12 min", "3 h", "6 d". */
    fun short(thenMs: Long, nowMs: Long): String {
        val s = ((nowMs - thenMs) / 1000).coerceAtLeast(0)
        return when {
            s < 45 -> "<1 min"
            s < 3_600 -> "${(s + 30) / 60} min"
            s < 86_400 -> "${s / 3_600} h"
            else -> "${s / 86_400} d"
        }
    }

    /** "6 days", "1 day", "5 hours": the age for the line "I know this request is 6 days old". */
    fun long(ageMs: Long): String {
        val s = (ageMs / 1000).coerceAtLeast(0)
        fun unit(n: Long, one: String) = if (n == 1L) "1 $one" else "$n ${one}s"
        return when {
            s < 3_600 -> unit(maxOf(1, (s + 30) / 60), "minute")
            s < 86_400 -> unit(s / 3_600, "hour")
            else -> unit(s / 86_400, "day")
        }
    }

    /** The age for a screen reader: "just now", "5 minutes ago", "3 hours ago", "yesterday", "6 days ago". No short forms. */
    fun spoken(thenMs: Long, nowMs: Long): String {
        val s = ((nowMs - thenMs) / 1000).coerceAtLeast(0)
        return when {
            s < 45 -> "just now"
            s < 86_400 -> long(s * 1000) + " ago"
            s < 172_800 -> "yesterday"
            else -> long(s * 1000) + " ago"
        }
    }

    private val CLOCK: DateTimeFormatter = DateTimeFormatter.ofPattern("HH:mm", Locale.US)

    /** "14:02" for a time of the same day as [nowMs], else "11-04 14:02". */
    fun stamp(ms: Long, nowMs: Long, zone: ZoneId): String {
        val then = Instant.ofEpochMilli(ms).atZone(zone)
        val now = Instant.ofEpochMilli(nowMs).atZone(zone)
        return if (then.toLocalDate() == now.toLocalDate()) CLOCK.format(then) else DAY_TIME.format(then)
    }

    private val DAY_TIME: DateTimeFormatter = DateTimeFormatter.ofPattern("MM-dd HH:mm", Locale.US)
    private val FULL: DateTimeFormatter = DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm", Locale.US)

    /** "11-14 08:00". */
    fun dayTime(ms: Long, zone: ZoneId): String = DAY_TIME.format(Instant.ofEpochMilli(ms).atZone(zone))

    /** "2026-11-14 08:00". */
    fun full(ms: Long, zone: ZoneId): String = FULL.format(Instant.ofEpochMilli(ms).atZone(zone))
}
