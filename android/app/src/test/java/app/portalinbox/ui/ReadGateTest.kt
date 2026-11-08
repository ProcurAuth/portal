package app.portalinbox.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ReadGateTest {
    private val key = "sha256:aa"

    @Test
    fun aNewGateIsLocked() {
        val g = ReadGate.start(key)
        assertTrue(g.locked)
        assertEquals(0, g.progressPercent)
    }

    @Test
    fun aCommandThatFitsInTheBoxIsReadAtOnce() {
        val g = ReadGate.observe(ReadGate.start(key), key, scrollY = 0, viewport = 400, content = 300)
        assertTrue(g.readToEnd)
        assertEquals(100, g.progressPercent)
    }

    @Test
    fun aLongCommandStaysLockedUntilTheEnd() {
        var g = ReadGate.start(key)
        g = ReadGate.observe(g, key, 0, 400, 1000)
        assertTrue(g.locked)
        assertEquals(40, g.progressPercent)
        g = ReadGate.observe(g, key, 300, 400, 1000)
        assertTrue(g.locked)
        assertEquals(70, g.progressPercent)
        g = ReadGate.observe(g, key, 598, 400, 1000)
        assertTrue(g.readToEnd)
    }

    @Test
    fun theToleranceAbsorbsTheRoundingOfTheScroll() {
        val almost = ReadGate.observe(ReadGate.start(key), key, 600 - ReadGate.TOLERANCE_PX, 400, 1000)
        assertTrue(almost.readToEnd)
        val short = ReadGate.observe(ReadGate.start(key), key, 600 - ReadGate.TOLERANCE_PX - 1, 400, 1000)
        assertTrue(short.locked)
    }

    @Test
    fun progressNeverShows100BeforeTheEnd() {
        val g = ReadGate.observe(ReadGate.start(key), key, 599 - ReadGate.TOLERANCE_PX, 400, 1000)
        assertEquals(99, g.progressPercent)
    }

    @Test
    fun onceReadTheGateStaysOpenWhenTheOwnerScrollsBack() {
        var g = ReadGate.observe(ReadGate.start(key), key, 600, 400, 1000)
        assertTrue(g.readToEnd)
        g = ReadGate.observe(g, key, 0, 400, 1000)
        assertTrue(g.readToEnd)
        assertEquals(100, g.progressPercent)
    }

    @Test
    fun aBoxThatIsNotMeasuredYetChangesNothing() {
        val g = ReadGate.start(key)
        assertEquals(g, ReadGate.observe(g, key, 0, 0, 0))
        assertEquals(g, ReadGate.observe(g, key, 0, 400, 0))
        assertEquals(g, ReadGate.observe(g, key, 0, 0, 400))
    }

    @Test
    fun aNewCommandResetsTheGate() {
        val read = ReadGate.observe(ReadGate.start(key), key, 600, 400, 1000)
        assertTrue(read.readToEnd)
        val other = ReadGate.observe(read, "sha256:bb", 0, 400, 1000)
        assertEquals("sha256:bb", other.key)
        assertFalse(other.readToEnd)
    }

    @Test
    fun aNegativeScrollCountsAsZero() {
        val g = ReadGate.observe(ReadGate.start(key), key, -50, 400, 1000)
        assertEquals(40, g.progressPercent)
    }

    @Test
    fun aSavedGateComesBackOnlyForTheSameCommand() {
        assertTrue(ReadGate.restore(key, key, true).readToEnd)
        assertFalse(ReadGate.restore(key, "other", true).readToEnd)
        assertFalse(ReadGate.restore(key, key, false).readToEnd)
        assertFalse(ReadGate.restore(key, null, true).readToEnd)
    }
}
