package app.portalinbox.data

import java.util.concurrent.ConcurrentHashMap

/**
 * The difference between the clock of each server and the clock of the phone (the age of an item counts with
 * `server_time_ms`). A `welcome` frame gives the server time. The offset is `server time - phone time` at that moment.
 * Without an offset (a server never seen since the start) the phone clock stands alone.
 */
class ClockOffsets {
    private val offsets = ConcurrentHashMap<Long, Long>()

    fun update(serverId: Long, serverTimeMs: Long, phoneNowMs: Long) {
        offsets[serverId] = serverTimeMs - phoneNowMs
    }

    fun set(serverId: Long, offsetMs: Long) {
        offsets[serverId] = offsetMs
    }

    fun offset(serverId: Long): Long? = offsets[serverId]

    /** The time of the server now, as far as the phone knows. */
    fun now(serverId: Long, phoneNowMs: Long): Long = phoneNowMs + (offsets[serverId] ?: 0L)

    fun forget(serverId: Long) {
        offsets.remove(serverId)
    }
}
