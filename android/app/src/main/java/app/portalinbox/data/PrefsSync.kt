package app.portalinbox.data

import app.portalinbox.proto.Prefs
import app.portalinbox.proto.QuietPrefs
import app.portalinbox.proto.SetPrefsFrame
import app.portalinbox.proto.WelcomeFrame

/** The edits of the owner that the server did not confirm yet, one value for each server. */
interface PendingPrefs {
    fun get(serverId: Long): Prefs?
    fun set(serverId: Long, prefs: Prefs)
    fun clear(serverId: Long)
}

/**
 * The preferences of the account (quiet hours, daily summary) between the phone and the server. Pure Kotlin.
 *
 * The server holds the values. A `set_prefs` frame has no answer, so the phone keeps its edit as *pending* until a `welcome`
 * shows the same values. At each `welcome`:
 * - nothing pending: the values of the `welcome` apply (another phone or the web may have changed them);
 * - pending equal to the `welcome`: the server has the edit, the pending value goes away;
 * - pending different: the server lost the edit (the frame never left, or the storage was away), so the phone sends it again.
 *
 * An edit of the owner wins over the server until the server confirms it: the last writer wins.
 */
object PrefsSync {
    sealed interface Action {
        /** Nothing to send. */
        data object None : Action

        /** Send this frame: the server does not have the edit. */
        data class Send(val frame: SetPrefsFrame) : Action
    }

    /** The values that apply on this phone: the pending edit, else the last `welcome`, else none. */
    fun effective(stored: Prefs?, pending: Prefs?): Prefs = pending ?: stored ?: Prefs.EMPTY

    /** The quiet hours that apply on this phone. */
    fun quiet(stored: Prefs?, pending: Prefs?): QuietPrefs? = effective(stored, pending).quiet

    /**
     * Decides what the phone does with a `welcome`. A server without items (`itemsEnabled` false) answers `not_enabled` to
     * `set_prefs`: the phone keeps the edit and sends nothing.
     */
    fun onWelcome(pending: PendingPrefs, serverId: Long, w: WelcomeFrame): Action {
        val mine = pending.get(serverId) ?: return Action.None
        if (mine == w.prefs) {
            pending.clear(serverId)
            return Action.None
        }
        return if (w.itemsEnabled) Action.Send(SetPrefsFrame(mine)) else Action.None
    }

    /** The owner changed a value. The edit is pending until the server confirms it. The frame to send is returned. */
    fun edit(pending: PendingPrefs, serverId: Long, prefs: Prefs): SetPrefsFrame {
        pending.set(serverId, prefs)
        return SetPrefsFrame(prefs)
    }
}
