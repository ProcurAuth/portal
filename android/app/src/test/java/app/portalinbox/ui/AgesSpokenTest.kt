package app.portalinbox.ui

import org.junit.Assert.assertEquals
import org.junit.Test
import java.time.ZoneOffset

class AgesSpokenTest {
    private val now = 1_788_000_000_000L
    private val min = 60_000L
    private val hour = 3_600_000L
    private val day = 86_400_000L

    @Test
    fun theSpokenAgeUsesFullWords() {
        assertEquals("just now", Ages.spoken(now - 20_000, now))
        assertEquals("1 minute ago", Ages.spoken(now - min, now))
        assertEquals("3 minutes ago", Ages.spoken(now - 3 * min, now))
        assertEquals("1 hour ago", Ages.spoken(now - hour, now))
        assertEquals("5 hours ago", Ages.spoken(now - 5 * hour, now))
        assertEquals("yesterday", Ages.spoken(now - 30 * hour, now))
        assertEquals("2 days ago", Ages.spoken(now - 2 * day, now))
        assertEquals("6 days ago", Ages.spoken(now - 6 * day, now))
    }

    @Test
    fun aTimeInTheFutureIsJustNow() {
        assertEquals("just now", Ages.spoken(now + hour, now))
    }

    @Test
    fun theStampShowsTheClockForToday() {
        val noon = java.time.LocalDateTime.parse("2026-11-08T12:00:00").toInstant(ZoneOffset.UTC).toEpochMilli()
        assertEquals("14:02", Ages.stamp(noon + 2 * hour + 2 * min, noon + 3 * hour, ZoneOffset.UTC))
    }

    @Test
    fun theStampShowsTheDateForAnotherDay() {
        val before = java.time.LocalDateTime.parse("2026-11-04T14:17:00").toInstant(ZoneOffset.UTC).toEpochMilli()
        assertEquals("11-04 14:17", Ages.stamp(before, before + 4 * day, ZoneOffset.UTC))
    }
}
