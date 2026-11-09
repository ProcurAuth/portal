package app.portalinbox.portal

import app.portalinbox.data.ItemEngine
import app.portalinbox.data.OpenCount
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class HomeFilterTest {
    private val ids = listOf(1L, 2L, 3L)

    @Test
    fun theFilterIsAllAtEachStart() {
        assertEquals(HomeFilter.All, HomeFilter.AT_START)
        assertNull(HomeFilter.AT_START.portalId)
    }

    @Test
    fun allShowsEveryPortalAndAPortalFilterShowsOnlyItself() {
        assertTrue(HomeFilter.All.matches(1))
        assertTrue(HomeFilter.All.matches(99))
        assertTrue(HomeFilter.Portal(2).matches(2))
        assertFalse(HomeFilter.Portal(2).matches(1))
        assertEquals(2L, HomeFilter.Portal(2).portalId)
    }

    @Test
    fun aRemovedPortalInTheFilterFallsBackToAll() {
        assertEquals(HomeFilter.All, HomeFilter.Portal(7).resolve(ids))
        assertEquals(HomeFilter.Portal(2), HomeFilter.Portal(2).resolve(ids))
        assertEquals(HomeFilter.All, HomeFilter.All.resolve(emptyList()))
        assertEquals(HomeFilter.All, HomeFilter.Portal(2).resolve(emptyList()))
    }

    @Test
    fun aTapChoosesAChipAndASecondTapOnAPortalGoesBackToAll() {
        assertEquals(HomeFilter.Portal(2), HomeFilter.tap(HomeFilter.All, HomeFilter.Portal(2)))
        assertEquals(HomeFilter.Portal(3), HomeFilter.tap(HomeFilter.Portal(2), HomeFilter.Portal(3)))
        assertEquals(HomeFilter.All, HomeFilter.tap(HomeFilter.Portal(2), HomeFilter.Portal(2)))
        assertEquals(HomeFilter.All, HomeFilter.tap(HomeFilter.Portal(2), HomeFilter.All))
        assertEquals(HomeFilter.All, HomeFilter.tap(HomeFilter.All, HomeFilter.All))
    }

    @Test
    fun theTitlesNameThePortalOnlyWithAFilter() {
        assertEquals("To answer", HomeFilter.sectionTitle("To answer", null))
        assertEquals("To answer · Production", HomeFilter.sectionTitle("To answer", "Production"))
        assertEquals("Nothing from Staging", HomeFilter.nothingFrom("Staging"))
    }

    // ------------------------------------------------------------ counts

    @Test
    fun aChipCountsUnreadAlertsPlusOpenItems() {
        val counts = HomeCounts.build(
            ids,
            open = mapOf(1L to OpenCount(approvals = 2, questions = 1), 2L to OpenCount(0, 1)),
            unreadAlerts = mapOf(1L to 1, 3L to 2),
        )
        assertEquals(4, counts.of(HomeFilter.Portal(1)))
        assertEquals(1, counts.of(HomeFilter.Portal(2)))
        assertEquals(2, counts.of(HomeFilter.Portal(3)))
        assertEquals(7, counts.of(HomeFilter.All))
        assertEquals(7, counts.all)
    }

    @Test
    fun aPortalWithNothingCountsZero() {
        val counts = HomeCounts.build(ids, emptyMap(), emptyMap())
        assertEquals(0, counts.of(HomeFilter.Portal(1)))
        assertEquals(0, counts.of(HomeFilter.Portal(42)))
        assertEquals(0, counts.all)
    }

    @Test
    fun theRowsOfARemovedPortalAreNotCounted() {
        val counts = HomeCounts.build(listOf(1L), mapOf(1L to OpenCount(1, 0), 9L to OpenCount(5, 5)), mapOf(9L to 4))
        assertEquals(1, counts.all)
        assertEquals(0, counts.of(HomeFilter.Portal(9)))
    }

    @Test
    fun theRepositoryCountsOpenItemsByKindForEachPortal() {
        val repo = MemoryItemRepository()
        val engine = ItemEngine(repo, ReferenceEffectHasher(), { 1_788_000_000_000L })
        engine.onRequest(1, Make.approval(Make.hex(1), Make.hex(0x11)))
        engine.onRequest(1, Make.approval(Make.hex(2), Make.hex(0x12)))
        engine.onRequest(1, Make.question(Make.hex(3), Make.hex(0x13)))
        engine.onRequest(2, Make.approval(Make.hex(1), Make.hex(0x14)))
        engine.onRequest(3, Make.approval(Make.hex(1), Make.hex(0x15)))
        engine.onSettled(3, Make.settled(Make.hex(1), Make.hex(0x16)))
        val counts = repo.openCounts()
        assertEquals(OpenCount(2, 1), counts[1L])
        assertEquals(OpenCount(1, 0), counts[2L])
        assertNull(counts[3L])
        assertEquals(3, counts[1L]!!.total)
    }
}
