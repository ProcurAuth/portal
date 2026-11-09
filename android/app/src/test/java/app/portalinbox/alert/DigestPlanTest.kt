package app.portalinbox.alert

import app.portalinbox.alert.DigestPlan.Known
import app.portalinbox.data.ItemEvent
import app.portalinbox.testsupport.Fixtures
import app.portalinbox.testsupport.Make
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The digest of the portals before it joins the one notification: what still waits, the merge, the memory of the last frames. */
class DigestPlanTest {
    private val day = 86_400_000L
    private val hour = 3_600_000L
    private val now = 1_788_000_000_000L

    private fun frame(server: Long, id: String, count: Long, oldestAgeMs: Long, ids: List<String>, receivedAgeMs: Long = 0) =
        DigestPlan.Frame(server, id, count, now - oldestAgeMs, ids, now - receivedAgeMs)

    // ------------------------------------------------------------ what still waits

    @Test
    fun theFrameCountIsUsedWhenThePhoneDoesNotKnowTheItemsYet() {
        val w = DigestPlan.waiting(frame(1, "d1", 3, 6 * day, listOf("a", "b", "c")), emptyMap())!!
        assertEquals(3, w.count)
        assertEquals(now - 6 * day, w.oldestCreatedAtMs)
    }

    @Test
    fun anIdThatThePhoneKnowsAsClosedLeavesTheCount() {
        val known = mapOf("a" to Known(false, now - 6 * day), "b" to Known(true, now - day))
        val w = DigestPlan.waiting(frame(1, "d1", 2, 6 * day, listOf("a", "b")), known)!!
        assertEquals(1, w.count)
        // The oldest item that still waits is the one of the day before.
        assertEquals(now - day, w.oldestCreatedAtMs)
    }

    @Test
    fun noDigestRemainsWhenEveryItemIsClosed() {
        val known = mapOf("a" to Known(false, now - day), "b" to Known(false, now - day))
        assertNull(DigestPlan.waiting(frame(1, "d1", 2, day, listOf("a", "b")), known))
    }

    @Test
    fun aClosedItemLeavesTheCountEvenWhenTheListIsNotComplete() {
        // The portal holds 5 open items and names the 2 oldest.
        val known = mapOf("a" to Known(false, now - 3 * day))
        val w = DigestPlan.waiting(frame(1, "d1", 5, 3 * day, listOf("a", "b")), known)!!
        assertEquals(4, w.count)
        // "b" is not known: the oldest time stays the one of the frame.
        assertEquals(now - 3 * day, w.oldestCreatedAtMs)
    }

    @Test
    fun theOldestTimeIsTheSmallestOfTheKnownOpenItems() {
        val known = mapOf(
            "a" to Known(true, now - 2 * day),
            "b" to Known(true, now - 5 * hour),
        )
        assertEquals(now - 2 * day, DigestPlan.waiting(frame(1, "d1", 2, 3 * day, listOf("a", "b")), known)!!.oldestCreatedAtMs)
    }

    @Test
    fun aFrameWithNoIdsStillCounts() {
        assertEquals(7, DigestPlan.waiting(frame(1, "d1", 7, day, emptyList()), emptyMap())!!.count)
    }

    // ------------------------------------------------------------ merge of several portals

    @Test
    fun fourPortalsAndThreeDigestsGiveThreeParts() {
        val frames = listOf(
            frame(1, "d1", 2, 6 * day, listOf("a", "b")),
            frame(3, "d3", 1, 3 * hour, listOf("c")),
            frame(4, "d4", 1, 9 * 60_000L, listOf("d")),
        )
        val merged = DigestPlan.merge(frames, { emptyMap() }, now)
        assertEquals(listOf(1L, 3L, 4L), merged.map { it.serverId })
        assertEquals(listOf(2L, 1L, 1L), merged.map { it.count })
    }

    @Test
    fun aPortalWhoseItemsAreAllClosedHasNoPart() {
        val frames = listOf(frame(1, "d1", 1, day, listOf("a")), frame(2, "d2", 1, day, listOf("b")))
        val known = { f: DigestPlan.Frame -> if (f.serverId == 1L) mapOf("a" to Known(false, now - day)) else emptyMap() }
        assertEquals(listOf(2L), DigestPlan.merge(frames, known, now).map { it.serverId })
    }

    @Test
    fun noPortalWithAnythingLeftGivesNoDigestAtAll() {
        assertTrue(DigestPlan.merge(emptyList(), { emptyMap() }, now).isEmpty())
        val known = { _: DigestPlan.Frame -> mapOf("a" to Known(false, now - day)) }
        assertTrue(DigestPlan.merge(listOf(frame(1, "d1", 1, day, listOf("a"))), known, now).isEmpty())
    }

    @Test
    fun aDigestIsAReminderThatLastsOneDay() {
        val fresh = frame(1, "d1", 1, day, listOf("a"), receivedAgeMs = DigestPlan.LIFETIME_MS - 1)
        val old = frame(2, "d2", 1, day, listOf("b"), receivedAgeMs = DigestPlan.LIFETIME_MS)
        assertEquals(listOf(1L), DigestPlan.merge(listOf(fresh, old), { emptyMap() }, now).map { it.serverId })
    }

    @Test
    fun theTextHasTheCountAndTheAgeOfTheOldest() {
        assertEquals("3 waiting, oldest 6 days", DigestPlan.text(3, 6 * day))
        assertEquals("1 waiting, oldest 5 hours", DigestPlan.text(1, 5 * hour))
    }

    // ------------------------------------------------------------ memory

    @Test
    fun theMemoryKeepsTheLastFrameOfEachPortal() {
        val m = DigestPlan.Memory()
        assertTrue(m.record(frame(1, "d1", 2, day, listOf("a", "b"))))
        assertTrue(m.record(frame(2, "d2", 1, day, listOf("c"))))
        assertTrue(m.record(frame(1, "d3", 4, day, emptyList())))
        val all = m.frames()
        assertEquals(2, all.size)
        assertEquals(4, all.first { it.serverId == 1L }.count)
    }

    @Test
    fun aDeliveryThatComesAgainIsNotNews() {
        val m = DigestPlan.Memory()
        assertTrue(m.record(frame(1, "d1", 2, day, emptyList())))
        assertFalse(m.record(frame(1, "d1", 2, day, emptyList())))
        assertEquals(1, m.frames().size)
    }

    @Test
    fun aRemovedPortalLeavesTheMemory() {
        val m = DigestPlan.Memory()
        m.record(frame(1, "d1", 2, day, emptyList()))
        m.record(frame(2, "d2", 2, day, emptyList()))
        m.forget(1)
        assertEquals(listOf(2L), m.frames().map { it.serverId })
    }

    @Test
    fun theGoldenDigestFrameMakesAPart() {
        val f = Make.digest(Make.hex(7))
        val e = ItemEvent.Digest(1, f.id, f.count, f.oldestCreatedAtMs, f.ids)
        val w = DigestPlan.waiting(DigestPlan.Frame(e.serverId, e.deliveryId, e.count, e.oldestCreatedAtMs, e.ids, now), emptyMap())!!
        assertEquals(3L, w.count)
        assertEquals(3L, Fixtures.golden("server", "digest.json").json.getLong("count"))
    }
}
