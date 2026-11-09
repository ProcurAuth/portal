package app.portalinbox.ui

/** The discussion that is on screen now. A turn of the agent for it makes no notification: the owner reads it. */
object OpenScreens {
    @Volatile private var thread: Pair<Long, String>? = null

    fun enterThread(serverId: Long, requestId: String) {
        thread = serverId to requestId
    }

    fun leaveThread(serverId: Long, requestId: String) {
        if (thread == serverId to requestId) thread = null
    }

    fun isThread(serverId: Long, requestId: String): Boolean = thread == serverId to requestId
}
