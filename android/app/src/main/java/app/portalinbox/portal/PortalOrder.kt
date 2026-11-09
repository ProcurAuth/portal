package app.portalinbox.portal

/** The order of the portals that the user sets by dragging (proposal 3.4). */
object PortalOrder {
    /**
     * The order to store. [current] are the ids of the portals now. [requested] is what the screen sends. An id that is not in
     * [current] is dropped, an id that comes twice counts once, and a portal that [requested] does not name goes last in its
     * old order. So the result always holds each current id exactly once.
     */
    fun merge(current: List<Long>, requested: List<Long>): List<Long> {
        val known = current.toSet()
        val head = requested.filter { it in known }.distinct()
        return head + current.filter { it !in head }
    }
}
