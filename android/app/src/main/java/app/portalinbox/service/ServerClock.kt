package app.portalinbox.service

import android.content.Context
import app.portalinbox.data.ClockOffsets

/**
 * The clock of each server, for the ages on the item screens (DIAL 4.8). The `welcome` frame gives the server time. The
 * offset also goes to a small file, so a phone that opens the app offline still shows the right age.
 */
object ServerClock {
    private val offsets = ClockOffsets()
    private const val FILE = "inbox_server_clock"

    fun update(context: Context, serverId: Long, serverTimeMs: Long) {
        offsets.update(serverId, serverTimeMs, System.currentTimeMillis())
        offsets.offset(serverId)?.let {
            context.getSharedPreferences(FILE, Context.MODE_PRIVATE).edit().putLong(serverId.toString(), it).apply()
        }
    }

    /** The time of the server now. Without a `welcome` seen since the install, the phone clock stands alone. */
    fun now(context: Context, serverId: Long): Long {
        if (offsets.offset(serverId) == null) {
            val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
            if (prefs.contains(serverId.toString())) offsets.set(serverId, prefs.getLong(serverId.toString(), 0L))
        }
        return offsets.now(serverId, System.currentTimeMillis())
    }

    fun forget(context: Context, serverId: Long) {
        offsets.forget(serverId)
        context.getSharedPreferences(FILE, Context.MODE_PRIVATE).edit().remove(serverId.toString()).apply()
    }
}
