package app.portalinbox.portal

import java.text.Normalizer
import java.util.Locale

/**
 * Folds a text to a skeleton, so that two texts that a person can mistake for each other give the same skeleton.
 * Pure Kotlin. Used for the uniqueness of the nicknames ([PortalNaming]) and for the look-alike check of addresses
 * ([LookAlike]).
 *
 * The folding does these steps: Unicode compatibility form (a full-width letter becomes a plain one), removal of
 * the accents, lower case, a short list of Cyrillic and Greek letters that look like Latin ones, then `0` as `o`
 * and `1`, `l` and `i` as one letter (the proposal merges `0/O` and `1/l/I`).
 */
object Confusables {
    private val LOOKALIKES: Map<Char, Char> = mapOf(
        // Cyrillic
        'а' to 'a', 'е' to 'e', 'о' to 'o', 'р' to 'p', 'с' to 'c', 'х' to 'x',
        'у' to 'y', 'і' to 'i', 'ј' to 'j', 'ѕ' to 's', 'ԁ' to 'd', 'һ' to 'h',
        'ӏ' to 'l', 'ԛ' to 'q', 'ԝ' to 'w', 'к' to 'k', 'т' to 't', 'н' to 'h',
        // Greek
        'ο' to 'o', 'α' to 'a', 'ν' to 'v', 'ρ' to 'p', 'ι' to 'i', 'κ' to 'k',
        'χ' to 'x', 'υ' to 'u', 'ε' to 'e', 'τ' to 't',
        // Other letters that pass for a Latin one
        'ı' to 'i', 'ł' to 'l', 'ø' to 'o', 'đ' to 'd',
    )

    /** The skeleton of [text]: see the class comment. It keeps spaces and separators: callers remove them. */
    fun skeleton(text: String): String {
        val decomposed = Normalizer.normalize(text, Normalizer.Form.NFKD)
        val out = StringBuilder(decomposed.length)
        for (ch in decomposed.lowercase(Locale.ROOT)) {
            if (Character.getType(ch) == Character.NON_SPACING_MARK.toInt()) continue
            val mapped = LOOKALIKES[ch] ?: ch
            out.append(
                when (mapped) {
                    '0' -> 'o'
                    '1', 'i', '|' -> 'l'
                    else -> mapped
                }
            )
        }
        return out.toString()
    }

    /**
     * The skeleton for a host name: [skeleton] plus `rn` as `m` and `vv` as `w`, which is how a sans-serif font draws them.
     */
    fun hostSkeleton(host: String): String = skeleton(host).replace("rn", "m").replace("vv", "w")

    /** The edit distance of two texts: insertion, deletion, substitution and the swap of two neighbours each cost 1. */
    fun distance(a: String, b: String): Int {
        val x = a.codePoints().toArray()
        val y = b.codePoints().toArray()
        if (x.isEmpty()) return y.size
        if (y.isEmpty()) return x.size
        val d = Array(x.size + 1) { IntArray(y.size + 1) }
        for (i in 0..x.size) d[i][0] = i
        for (j in 0..y.size) d[0][j] = j
        for (i in 1..x.size) {
            for (j in 1..y.size) {
                val cost = if (x[i - 1] == y[j - 1]) 0 else 1
                var best = minOf(d[i - 1][j] + 1, d[i][j - 1] + 1, d[i - 1][j - 1] + cost)
                if (i > 1 && j > 1 && x[i - 1] == y[j - 2] && x[i - 2] == y[j - 1]) best = minOf(best, d[i - 2][j - 2] + 1)
                d[i][j] = best
            }
        }
        return d[x.size][y.size]
    }

    /**
     * The places (UTF-16 indexes) of the characters of [text] that differ from [other], for a screen that marks the
     * changed letters of an address. An aligned pair that is equal is not marked. A substituted or added character is.
     */
    fun changedPositions(text: String, other: String): Set<Int> {
        val x = text.codePoints().toArray()
        val y = other.codePoints().toArray()
        val d = Array(x.size + 1) { IntArray(y.size + 1) }
        for (i in 0..x.size) d[i][0] = i
        for (j in 0..y.size) d[0][j] = j
        for (i in 1..x.size) {
            for (j in 1..y.size) {
                val cost = if (x[i - 1] == y[j - 1]) 0 else 1
                d[i][j] = minOf(d[i - 1][j] + 1, d[i][j - 1] + 1, d[i - 1][j - 1] + cost)
            }
        }
        // Index in `text` of each code point.
        val starts = IntArray(x.size)
        var at = 0
        for (k in x.indices) {
            starts[k] = at
            at += Character.charCount(x[k])
        }
        val changed = HashSet<Int>()
        var i = x.size
        var j = y.size
        while (i > 0 || j > 0) {
            when {
                i > 0 && j > 0 && x[i - 1] == y[j - 1] && d[i][j] == d[i - 1][j - 1] -> { i--; j-- }
                i > 0 && j > 0 && d[i][j] == d[i - 1][j - 1] + 1 -> { markAll(changed, starts, x, i - 1); i--; j-- }
                i > 0 && d[i][j] == d[i - 1][j] + 1 -> { markAll(changed, starts, x, i - 1); i-- }
                else -> j--
            }
        }
        return changed
    }

    private fun markAll(into: MutableSet<Int>, starts: IntArray, cps: IntArray, k: Int) {
        for (n in 0 until Character.charCount(cps[k])) into += starts[k] + n
    }
}
