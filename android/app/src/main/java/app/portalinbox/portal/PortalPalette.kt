package app.portalinbox.portal

import kotlin.math.pow

/**
 * The 10 colors of the portals (proposal 3.1). Pure Kotlin. The order is the order in which the app proposes a free color:
 * violet, teal, pink, sand, sky, lime, slate, coral, moss, rose. The index in this list is the value of the column `color`.
 *
 * Each color has a value for the dark theme and one for the light theme. Each was checked for a contrast of at least
 * 4.5 : 1 against the card and the page of its theme (`PortalPaletteTest` repeats the check from `colors.xml`). The palette
 * avoids the colors of the states: danger red, warning amber and the primary periwinkle.
 */
object PortalPalette {
    class Entry(val name: String, val dark: Int, val light: Int)

    private fun e(name: String, dark: Long, light: Long) = Entry(name, (0xFF000000L or dark).toInt(), (0xFF000000L or light).toInt())

    val entries: List<Entry> = listOf(
        e("violet", 0xA78BFA, 0x6D28D9),
        e("teal", 0x2DD4BF, 0x0F766E),
        e("pink", 0xF472B6, 0xBE185D),
        e("sand", 0xD6B27A, 0x8A5A12),
        e("sky", 0x38BDF8, 0x0369A1),
        e("lime", 0xA3E635, 0x3F6212),
        e("slate", 0xCBD5E1, 0x475569),
        e("coral", 0xFB923C, 0x9A3412),
        e("moss", 0x86EFAC, 0x166534),
        e("rose", 0xFDA4AF, 0x9F1239),
    )

    const val SIZE = 10

    /** The color of a text or a shape on a portal color: the dark page color on the dark theme, white on the light theme. */
    private const val ON_DARK_THEME = 0xFF0B0E13.toInt()
    private const val ON_LIGHT_THEME = 0xFFFFFFFF.toInt()

    /** Pairs of colors that look alike. The app does not give one of them right after the other. */
    private val ALIKE = listOf(setOf(2, 9), setOf(5, 8), setOf(3, 7))

    fun isValid(index: Int): Boolean = index in 0 until SIZE

    fun name(index: Int): String = entries[index].name

    /** ARGB of the dark theme. */
    fun dark(index: Int): Int = entries[index].dark

    /** ARGB of the light theme. */
    fun light(index: Int): Int = entries[index].light

    fun color(index: Int, night: Boolean): Int = if (night) dark(index) else light(index)

    fun onColor(night: Boolean): Int = if (night) ON_DARK_THEME else ON_LIGHT_THEME

    fun looksAlike(a: Int, b: Int): Boolean = setOf(a, b) in ALIKE

    /**
     * The color to propose: the first one of [entries] that is not in [used]. [last] is the color that the app gave to the
     * portal that was added most recently: a color that looks like it is skipped, unless nothing else is free. Returns
     * null when all 10 are used. An index outside 0 to 9 in [used] is ignored.
     */
    fun nextFree(used: Collection<Int>, last: Int? = null): Int? {
        val free = (0 until SIZE).filter { it !in used }
        if (free.isEmpty()) return null
        if (last != null) free.firstOrNull { !looksAlike(it, last) }?.let { return it }
        return free.first()
    }

    /** The WCAG contrast ratio of two opaque ARGB colors, 1.0 to 21.0. */
    fun contrast(a: Int, b: Int): Double {
        val la = luminance(a)
        val lb = luminance(b)
        return (maxOf(la, lb) + 0.05) / (minOf(la, lb) + 0.05)
    }

    private fun luminance(argb: Int): Double {
        fun channel(v: Int): Double {
            val c = v / 255.0
            return if (c <= 0.03928) c / 12.92 else ((c + 0.055) / 1.055).pow(2.4)
        }
        return 0.2126 * channel(argb shr 16 and 0xFF) + 0.7152 * channel(argb shr 8 and 0xFF) + 0.0722 * channel(argb and 0xFF)
    }

    /** [top] drawn over the opaque [bottom], as opaque ARGB. Used for a card with a little transparency. */
    fun over(top: Int, bottom: Int): Int {
        val a = (top ushr 24) / 255.0
        fun mix(shift: Int): Int {
            val t = (top shr shift) and 0xFF
            val b = (bottom shr shift) and 0xFF
            return Math.round(t * a + b * (1 - a)).toInt()
        }
        return (0xFF shl 24) or (mix(16) shl 16) or (mix(8) shl 8) or mix(0)
    }
}
