package app.portalinbox.service

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class BackoffTest {
    private var now = 1_000_000L

    /** No jitter in the first tests: the delay is the whole base. */
    private fun backoff(jitter: (Long) -> Long = { it }) = Backoff(now = { now }, jitter = jitter)

    @Test
    fun theDelayDoublesFromOneSecondAndStopsAtThirtySeconds() {
        val b = backoff()
        assertEquals(listOf(1_000L, 2_000L, 4_000L, 8_000L, 16_000L, 30_000L, 30_000L), List(7) { b.next() })
        assertEquals(7, b.failures)
        assertEquals(30_000L, b.capMs())
    }

    @Test
    fun theCapStaysThirtySecondsDuringTheFirstHour() {
        val b = backoff()
        repeat(5) { b.next() }
        now += Backoff.SLOW_AFTER_MS - 1
        assertEquals(30_000L, b.capMs())
        assertEquals(30_000L, b.next())
    }

    @Test
    fun afterOneHourOfFailuresTheCapIsFiveMinutes() {
        val b = backoff()
        b.next()
        now += Backoff.SLOW_AFTER_MS
        assertEquals(300_000L, b.capMs())
        // The delay grows by doubling up to the cap: 2 s now, then 4, 8, ... 256 s, then 300 s.
        val delays = List(12) { b.next() }
        assertEquals(2_000L, delays.first())
        assertEquals(300_000L, delays.last())
        assertTrue(delays.zipWithNext().all { (a, c) -> c >= a })
        assertTrue(delays.all { it <= 300_000L })
    }

    @Test
    fun successStartsTheCountAndTheHourAgain() {
        val b = backoff()
        repeat(10) { b.next() }
        now += Backoff.SLOW_AFTER_MS * 2
        assertEquals(300_000L, b.capMs())
        b.reset()
        assertEquals(0, b.failures)
        assertEquals(30_000L, b.capMs())
        assertEquals(1_000L, b.next())
        // The new hour counts from the first failure after the success, not from the old one.
        now += Backoff.SLOW_AFTER_MS - 1
        assertEquals(30_000L, b.capMs())
    }

    @Test
    fun noFailureMeansTheFastCapEvenAfterALongTime() {
        val b = backoff()
        now += Backoff.SLOW_AFTER_MS * 10
        assertEquals(30_000L, b.capMs())
    }

    @Test
    fun theDelayIsBetweenHalfOfTheBaseAndTheBase() {
        val low = backoff { 0 }
        assertEquals(500L, low.next())
        val high = backoff { it }
        assertEquals(1_000L, high.next())
        val random = Backoff(now = { now })
        repeat(50) {
            val d = random.next()
            assertTrue(d in 500L..30_000L)
        }
    }

    @Test
    fun theCountIsBoundedSoTheShiftNeverOverflows() {
        val b = backoff()
        repeat(1_000) { b.next() }
        assertEquals(16, b.failures)
        assertEquals(30_000L, b.next())
    }
}
