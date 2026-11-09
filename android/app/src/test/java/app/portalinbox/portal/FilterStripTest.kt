package app.portalinbox.portal

import app.portalinbox.testsupport.Portals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class FilterStripTest {
    private fun portal(id: Long, nickname: String, health: PortalHealth = PortalHealth.OK, isNew: Boolean = false) =
        HomePortal(Portals.identity(id, nickname, id.toInt() - 1), health, isNew, null, null)

    private val four = listOf(
        portal(1, "Production"), portal(2, "Staging"), portal(3, "Homelab", PortalHealth.OFFLINE), portal(4, "Client A", PortalHealth.KEY_CHANGED),
    )
    private val counts = HomeCounts(mapOf(1L to 3, 2L to 1, 3L to 1, 4L to 2), 7)

    @Test
    fun theStripStartsWithAllThenOneChipPerPortalInOrder() {
        val chips = FilterStrip.chips(four, counts, HomeFilter.All)
        assertEquals(listOf("All", "Production", "Staging", "Homelab", "Client A"), chips.map { it.portal?.identity?.nickname ?: "All" })
        assertEquals(listOf(7, 3, 1, 1, 2), chips.map { it.count })
        assertEquals(listOf(true, false, false, false, false), chips.map { it.selected })
        assertNull(chips.first().portal)
    }

    @Test
    fun theChipOfTheFilterIsSelected() {
        val chips = FilterStrip.chips(four, counts, HomeFilter.Portal(2))
        assertEquals(listOf(false, false, true, false, false), chips.map { it.selected })
    }

    @Test
    fun aPortalThatIsDownShowsADashedChip() {
        val chips = FilterStrip.chips(four, counts, HomeFilter.All)
        assertEquals(listOf(false, false, false, true, true), chips.map { it.dashed })
    }

    @Test
    fun withOnePortalOrNoneThereIsNoStrip() {
        assertTrue(FilterStrip.chips(emptyList(), HomeCounts(emptyMap(), 0), HomeFilter.All).isEmpty())
        assertTrue(FilterStrip.chips(listOf(portal(1, "Production")), counts, HomeFilter.All).isEmpty())
    }

    @Test
    fun oneDotPerPortalHollowWhenDown() {
        val dots = FilterStrip.dots(four)
        assertEquals(4, dots.size)
        assertEquals(listOf(false, false, true, true), dots.map { it.hollow })
        assertEquals(listOf(0, 1, 2, 3), dots.map { it.identity.colorIndex })
    }

    @Test
    fun theWordsForAScreenReaderNameThePortalTheStateAndTheNumber() {
        val chips = FilterStrip.chips(listOf(portal(1, "Production", isNew = true), portal(2, "Homelab", PortalHealth.OFFLINE)), counts, HomeFilter.Portal(1))
        assertEquals("All portals, 7 waiting", chips[0].spoken())
        assertEquals("Production, new, 3 waiting, selected", chips[1].spoken())
        assertEquals("Homelab, offline, 1 waiting", chips[2].spoken())
        assertEquals("Production connected. Staging connected. Homelab offline. Client A key check failed", FilterStrip.dotsSpoken(four))
        assertEquals("All portals connected", FilterStrip.dotsSpoken(listOf(portal(1, "Production"))))
        assertEquals("No portal", FilterStrip.dotsSpoken(emptyList()))
    }
}
