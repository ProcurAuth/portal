package app.portalinbox.service

import kotlin.random.Random

/**
 * The delay before a portal connection tries again (proposal 3.11). Pure Kotlin. One instance for each connection.
 *
 * The delay grows from 1 s and doubles at each failure. The cap is 30 s during the first hour of failures. After one hour
 * without a success the cap is 5 minutes: a portal that is down for long does not wake the radio every 30 s for ever. A
 * success ([reset]) starts the count again. The delay is jittered between half of the base and the base.
 *
 * [now] and [jitter] are injected so that a test needs no sleep and no random number.
 */
class Backoff(
    private val now: () -> Long = System::currentTimeMillis,
    private val jitter: (bound: Long) -> Long = { Random.nextLong(it + 1) },
) {
    companion object {
        const val FAST_CAP_MS = 30_000L
        const val SLOW_CAP_MS = 5 * 60_000L

        /** How long failures last before the slow cap applies. */
        const val SLOW_AFTER_MS = 60 * 60_000L
        private const val MAX_FAILURES = 16
        private const val FIRST_MS = 1_000L
    }

    /** The number of failures since the last success (0 after [reset]). */
    var failures = 0
        private set
    private var firstFailureMs = 0L

    /** The cap that applies now. */
    fun capMs(): Long = if (failures > 0 && now() - firstFailureMs >= SLOW_AFTER_MS) SLOW_CAP_MS else FAST_CAP_MS

    /** Counts a failure and returns the delay before the next try. */
    fun next(): Long {
        if (failures == 0) firstFailureMs = now()
        failures = minOf(failures + 1, MAX_FAILURES)
        val base = minOf(capMs(), FIRST_MS shl (failures - 1))
        return base / 2 + jitter(base / 2)
    }

    /** A session was welcomed: the count and the hour start again. */
    fun reset() {
        failures = 0
        firstFailureMs = 0L
    }
}
