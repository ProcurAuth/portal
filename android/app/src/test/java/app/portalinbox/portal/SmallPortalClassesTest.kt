package app.portalinbox.portal

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class LastSyncTest {
    @Test
    fun theFirstFrameIsWritten() = assertTrue(LastSync.shouldWrite(null, 5_000, force = false))

    @Test
    fun aFrameWithinAMinuteIsNotWritten() {
        assertFalse(LastSync.shouldWrite(10_000, 10_000, force = false))
        assertFalse(LastSync.shouldWrite(10_000, 69_999, force = false))
    }

    @Test
    fun aFrameAMinuteLaterIsWritten() {
        assertTrue(LastSync.shouldWrite(10_000, 70_000, force = false))
    }

    @Test
    fun aForcedWriteAlwaysGoes() = assertTrue(LastSync.shouldWrite(10_000, 10_001, force = true))

    @Test
    fun aClockThatWentBackIsWritten() = assertTrue(LastSync.shouldWrite(100_000, 50_000, force = false))
}

class PortalOrderTest {
    @Test
    fun theRequestedOrderApplies() {
        assertEquals(listOf(3L, 1L, 2L), PortalOrder.merge(listOf(1, 2, 3), listOf(3, 1, 2)))
    }

    @Test
    fun anUnknownIdIsDroppedAndADuplicateCountsOnce() {
        assertEquals(listOf(2L, 1L), PortalOrder.merge(listOf(1, 2), listOf(2, 99, 2, 1, 1)))
    }

    @Test
    fun aPortalThatTheRequestForgetsGoesLastInItsOldOrder() {
        assertEquals(listOf(3L, 1L, 2L, 4L), PortalOrder.merge(listOf(1, 2, 3, 4), listOf(3, 1)))
    }

    @Test
    fun anEmptyRequestKeepsTheOrder() {
        assertEquals(listOf(1L, 2L), PortalOrder.merge(listOf(1, 2), emptyList()))
        assertEquals(emptyList<Long>(), PortalOrder.merge(emptyList(), listOf(1)))
    }
}

class ConfusablesTest {
    @Test
    fun distanceCountsSwapsAsOne() {
        assertEquals(0, Confusables.distance("abc", "abc"))
        assertEquals(1, Confusables.distance("abc", "acb"))
        assertEquals(1, Confusables.distance("abc", "abcd"))
        assertEquals(3, Confusables.distance("", "abc"))
        assertEquals(3, Confusables.distance("abc", ""))
        assertEquals(3, Confusables.distance("kitten", "sitting"))
    }

    @Test
    fun skeletonFoldsTheListedConfusables() {
        assertEquals(Confusables.skeleton("Oo0"), "ooo")
        assertEquals(Confusables.skeleton("Il1|i"), "lllll")
        assertEquals("cafe", Confusables.skeleton("CAFÉ"))
    }

    @Test
    fun hostSkeletonAlsoFoldsRnAndVv() {
        assertEquals(Confusables.hostSkeleton("mail"), Confusables.hostSkeleton("rnail"))
        assertEquals(Confusables.hostSkeleton("wave"), Confusables.hostSkeleton("vvave"))
    }
}
