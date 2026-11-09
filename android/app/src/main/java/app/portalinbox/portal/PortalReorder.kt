package app.portalinbox.portal

/** The moves of the drag handles of the portal list (proposal 3.4). Pure Kotlin. The result goes to `Store.reorder`. */
object PortalReorder {
    /** [order] with [id] moved to [toIndex] (clamped to the list). An id that is not in the list gives the same list. */
    fun move(order: List<Long>, id: Long, toIndex: Int): List<Long> {
        val from = order.indexOf(id)
        if (from < 0) return order
        val rest = order.toMutableList().also { it.removeAt(from) }
        rest.add(toIndex.coerceIn(0, rest.size), id)
        return rest
    }

    /** One place up (a screen reader action). The first portal stays. */
    fun up(order: List<Long>, id: Long): List<Long> = move(order, id, order.indexOf(id) - 1)

    /** One place down. The last portal stays. */
    fun down(order: List<Long>, id: Long): List<Long> = move(order, id, order.indexOf(id) + 1)

    /**
     * The index that a dragged row takes. [tops] and [heights] are the positions and the heights of the rows in the current
     * order, in pixels. [centerY] is the middle of the dragged row. The row takes the place of the row whose middle is the
     * closest to [centerY].
     */
    fun targetIndex(tops: List<Int>, heights: List<Int>, centerY: Float): Int {
        if (tops.isEmpty()) return 0
        var best = 0
        var bestDistance = Float.MAX_VALUE
        for (i in tops.indices) {
            val d = kotlin.math.abs(tops[i] + heights[i] / 2f - centerY)
            if (d < bestDistance) { bestDistance = d; best = i }
        }
        return best
    }
}
