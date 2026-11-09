package app.portalinbox.portal

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PortalPaletteTest {
    /** The colors of `res/values/colors.xml` (light) or `res/values-night/colors.xml` (dark), as ARGB. */
    private fun resources(path: String): Map<String, Int> {
        val re = Regex("""<color name="([a-z_0-9]+)">#([0-9A-Fa-f]{6,8})</color>""")
        return re.findAll(File(path).readText()).associate { m ->
            val hex = m.groupValues[2]
            m.groupValues[1] to (if (hex.length == 6) 0xFF000000L or hex.toLong(16) else hex.toLong(16)).toInt()
        }
    }

    private val light = resources("src/main/res/values/colors.xml")
    private val night = resources("src/main/res/values-night/colors.xml")

    /** The page, and the card drawn over the page (the card has a little transparency). */
    private fun backgrounds(theme: Map<String, Int>): List<Int> {
        val page = theme.getValue("bg_page")
        return listOf(page, PortalPalette.over(theme.getValue("card"), page))
    }

    @Test
    fun hasTenEntriesWithTheNamesOfTheProposal() {
        assertEquals(10, PortalPalette.SIZE)
        assertEquals(10, PortalPalette.entries.size)
        assertEquals(
            listOf("violet", "teal", "pink", "sand", "sky", "lime", "slate", "coral", "moss", "rose"),
            PortalPalette.entries.map { it.name },
        )
        assertEquals(10, PortalPalette.entries.map { it.dark }.toSet().size)
        assertEquals(10, PortalPalette.entries.map { it.light }.toSet().size)
    }

    @Test
    fun valuesAreTheOnesOfTheProposal() {
        assertEquals(0xFFA78BFA.toInt(), PortalPalette.dark(0))
        assertEquals(0xFF6D28D9.toInt(), PortalPalette.light(0))
        assertEquals(0xFFFDA4AF.toInt(), PortalPalette.dark(9))
        assertEquals(0xFF9F1239.toInt(), PortalPalette.light(9))
        assertEquals(PortalPalette.dark(4), PortalPalette.color(4, night = true))
        assertEquals(PortalPalette.light(4), PortalPalette.color(4, night = false))
        assertEquals("sky", PortalPalette.name(4))
    }

    @Test
    fun everyColorHasAContrastOf4Point5AgainstThePageAndTheCardOfItsTheme() {
        for (i in 0 until PortalPalette.SIZE) {
            for (bg in backgrounds(night)) {
                val c = PortalPalette.contrast(PortalPalette.dark(i), bg)
                assertTrue("${PortalPalette.name(i)} dark on #%08X is %.2f".format(bg, c), c >= 4.5)
            }
            for (bg in backgrounds(light)) {
                val c = PortalPalette.contrast(PortalPalette.light(i), bg)
                assertTrue("${PortalPalette.name(i)} light on #%08X is %.2f".format(bg, c), c >= 4.5)
            }
        }
    }

    @Test
    fun theTextOnAPortalColorIsReadable() {
        for (i in 0 until PortalPalette.SIZE) {
            val dark = PortalPalette.contrast(PortalPalette.onColor(night = true), PortalPalette.dark(i))
            val light = PortalPalette.contrast(PortalPalette.onColor(night = false), PortalPalette.light(i))
            assertTrue("${PortalPalette.name(i)} dark monogram %.2f".format(dark), dark >= 4.5)
            assertTrue("${PortalPalette.name(i)} light monogram %.2f".format(light), light >= 4.5)
        }
    }

    @Test
    fun contrastOfBlackAndWhiteIs21() {
        assertEquals(21.0, PortalPalette.contrast(0xFF000000.toInt(), 0xFFFFFFFF.toInt()), 0.001)
        assertEquals(1.0, PortalPalette.contrast(0xFF123456.toInt(), 0xFF123456.toInt()), 0.001)
    }

    @Test
    fun overBlendsATransparentColor() {
        assertEquals(0xFF808080.toInt(), PortalPalette.over(0x80FFFFFF.toInt(), 0xFF000000.toInt()))
        assertEquals(0xFFFFFFFF.toInt(), PortalPalette.over(0xFFFFFFFF.toInt(), 0xFF000000.toInt()))
        assertEquals(0xFF000000.toInt(), PortalPalette.over(0x00FFFFFF, 0xFF000000.toInt()))
    }

    @Test
    fun nextFreeFollowsTheOrderOfTheProposal() {
        assertEquals(0, PortalPalette.nextFree(emptyList()))
        assertEquals(1, PortalPalette.nextFree(listOf(0)))
        assertEquals(2, PortalPalette.nextFree(listOf(0, 1)))
        assertEquals(3, PortalPalette.nextFree(listOf(0, 1, 2)))
    }

    @Test
    fun nextFreeFillsAHoleAndWrapsToTheStart() {
        // A hole in the middle.
        assertEquals(5, PortalPalette.nextFree(listOf(0, 1, 2, 3, 4, 6, 7, 8, 9)))
        // Only the first color is free, although all the later ones are used: the search comes back to the start.
        assertEquals(0, PortalPalette.nextFree((1..9).toList()))
        // The order of the list does not matter.
        assertEquals(2, PortalPalette.nextFree(listOf(1, 0)))
    }

    @Test
    fun nextFreeGivesNullWhenAllTenAreUsed() {
        assertNull(PortalPalette.nextFree((0..9).toList()))
    }

    @Test
    fun nextFreeIgnoresIndexesOutsideTheRange() {
        assertEquals(0, PortalPalette.nextFree(listOf(-1, 10, 99)))
    }

    @Test
    fun aColorThatLooksLikeTheLastOneIsNotGivenRightAfterIt() {
        // Pink (2) is used and sand (3) is the first free: sand and coral (7) look alike, pink and rose (9) too.
        // The last color was coral: sand is skipped for the next free one (sky, 4).
        assertEquals(4, PortalPalette.nextFree(listOf(0, 1, 2, 7), last = 7))
        // The last color was pink: sand does not look like pink, so it stays first.
        assertEquals(3, PortalPalette.nextFree(listOf(0, 1, 2), last = 2))
        // Lime (5) was the last, moss (8) looks alike: with slate (6) free, slate comes before moss.
        assertEquals(6, PortalPalette.nextFree(listOf(0, 1, 2, 3, 4, 5), last = 5))
    }

    @Test
    fun aColorThatLooksLikeTheLastOneIsGivenWhenNothingElseIsFree() {
        // Only rose (9) is free and the last was pink (2).
        assertEquals(9, PortalPalette.nextFree((0..8).toList(), last = 2))
        // Only sand (3) is free and the last was coral (7).
        assertEquals(3, PortalPalette.nextFree((0..9).filter { it != 3 }, last = 7))
    }

    @Test
    fun theThreeAlikePairsAreSymmetric() {
        for ((a, b) in listOf(2 to 9, 5 to 8, 3 to 7)) {
            assertTrue(PortalPalette.looksAlike(a, b))
            assertTrue(PortalPalette.looksAlike(b, a))
        }
        assertFalse(PortalPalette.looksAlike(0, 1))
        assertFalse(PortalPalette.looksAlike(2, 2))
    }

    @Test
    fun isValidIsZeroToNine() {
        assertTrue(PortalPalette.isValid(0))
        assertTrue(PortalPalette.isValid(9))
        assertFalse(PortalPalette.isValid(-1))
        assertFalse(PortalPalette.isValid(10))
    }
}
