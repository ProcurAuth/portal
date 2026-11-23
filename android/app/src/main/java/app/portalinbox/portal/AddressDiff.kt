package app.portalinbox.portal

/**
 * Finds the letters of an address that differ from the address of a portal that the phone holds, so the confirmation screen
 * can mark them (`porta[1].example.org` against `portal.example.org`). Pure Kotlin.
 *
 * The method is an edit distance with a trace back. It marks a letter of the shown text when it replaces a letter of the other
 * text, or when the other text has no such letter. When the shown text lacks a letter that the other text has, the letter after
 * the gap is marked (or the last letter, at the end), so a missing letter is never invisible.
 */
object AddressDiff {
    /** The ranges of [shown] to mark. The ranges are in order, never touch each other, and use the indices of [shown]. Empty when both texts are equal. */
    fun changed(shown: String, other: String): List<IntRange> {
        if (shown == other || shown.isEmpty()) return emptyList()
        val n = shown.length
        val m = other.length
        // cost[i][j] = distance of the first i letters of shown and the first j letters of other.
        val cost = Array(n + 1) { IntArray(m + 1) }
        for (i in 0..n) cost[i][0] = i
        for (j in 0..m) cost[0][j] = j
        for (i in 1..n) {
            for (j in 1..m) {
                val same = if (shown[i - 1] == other[j - 1]) 0 else 1
                cost[i][j] = minOf(cost[i - 1][j - 1] + same, cost[i - 1][j] + 1, cost[i][j - 1] + 1)
            }
        }
        val marked = BooleanArray(n)
        var i = n
        var j = m
        while (i > 0 || j > 0) {
            when {
                i > 0 && j > 0 && shown[i - 1] == other[j - 1] && cost[i][j] == cost[i - 1][j - 1] -> { i--; j-- }
                i > 0 && j > 0 && cost[i][j] == cost[i - 1][j - 1] + 1 -> { marked[i - 1] = true; i--; j-- }
                i > 0 && cost[i][j] == cost[i - 1][j] + 1 -> { marked[i - 1] = true; i-- }
                else -> {
                    // The other text has a letter that the shown text lacks: mark the letter after the gap.
                    marked[minOf(i, n - 1)] = true
                    j--
                }
            }
        }
        val out = mutableListOf<IntRange>()
        var start = -1
        for (k in 0..n) {
            val on = k < n && marked[k]
            if (on && start < 0) start = k
            if (!on && start >= 0) { out.add(start until k); start = -1 }
        }
        return out
    }
}
