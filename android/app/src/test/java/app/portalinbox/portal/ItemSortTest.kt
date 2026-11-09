package app.portalinbox.portal

import app.portalinbox.data.Item
import app.portalinbox.data.ItemEngine
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.junit.Assert.assertEquals
import org.junit.Test

class ItemSortTest {
    private val day = 86_400_000L
    private val hour = 3_600_000L
    private val now = 1_788_000_000_000L + 20 * day
    private val repo = MemoryItemRepository()
    private var arrival = now
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { arrival })
    private var n = 0

    /** An approval of [serverId] with the claimed creation time [createdAtMs], received at [receivedAtMs]. */
    private fun approval(serverId: Long, createdAtMs: Long, receivedAtMs: Long = now): Item {
        n++
        arrival = receivedAtMs
        engine.onRequest(serverId, Make.approval(Make.hex(0x100 + n), Make.hex(0x200 + n), createdAtMs = createdAtMs))
        return repo.item(serverId, Make.hex(0x100 + n))!!
    }

    private fun ids(items: List<Item>) = items.map { it.requestId }

    // ------------------------------------------------------------ the key

    @Test
    fun anHonestItemKeepsItsCreationTime() {
        assertEquals(now - 3 * hour, ItemSort.key(now - 3 * hour, 0, now))
    }

    @Test
    fun aForgedZeroTimeIsLimitedToSevenDaysBeforeTheArrival() {
        assertEquals(now - 7 * day, ItemSort.key(0, 0, now))
    }

    @Test
    fun aTimeInTheFutureIsLimitedToTheArrival() {
        assertEquals(now, ItemSort.key(now + 365 * day, 0, now))
        assertEquals(now, ItemSort.key(Long.MAX_VALUE, 0, now))
    }

    @Test
    fun theOffsetMovesAServerTimeToThePhoneClock() {
        // The clock of the portal is 2 hours ahead of the phone: a claimed time of "now + 2 h" is "now" on the phone.
        assertEquals(now - hour, ItemSort.key(now + hour, 2 * hour, now))
        // The clock of the portal is 2 hours behind: a claimed time of "now - 5 h" is "now - 3 h" on the phone.
        assertEquals(now - 3 * hour, ItemSort.key(now - 5 * hour, -2 * hour, now))
    }

    @Test
    fun anExtremeOffsetOrTimeDoesNotOverflow() {
        assertEquals(now - 7 * day, ItemSort.key(Long.MIN_VALUE, Long.MAX_VALUE, now))
        assertEquals(now, ItemSort.key(Long.MAX_VALUE, Long.MIN_VALUE, now))
    }

    // ------------------------------------------------------------ the list "To answer"

    @Test
    fun theOldestItemOfAllPortalsComesFirst() {
        val a = approval(1, now - 2 * hour)
        val b = approval(2, now - 5 * hour)
        val c = approval(3, now - 1 * hour)
        assertEquals(ids(listOf(b, a, c)), ids(ItemSort.toAnswer(listOf(a, b, c), emptyMap())))
    }

    @Test
    fun aRoguePortalCannotPutItsItemAboveAnHonestOneByForgingTheTime() {
        val honest = approval(1, now - 3 * day, receivedAtMs = now - 3 * day)
        val rogue = approval(2, 0)
        // The forged 0 counts as "7 days before the arrival", so it is older than the honest item of 3 days. It gains 4 days at
        // most, not 20: the honest item of 9 days that follows still stays above it.
        assertEquals(ids(listOf(rogue, honest)), ids(ItemSort.toAnswer(listOf(honest, rogue), emptyMap())))
        // An honest item older than the limit stays above the forged one.
        val older = approval(3, now - 9 * day, receivedAtMs = now - 9 * day)
        assertEquals(ids(listOf(older, rogue, honest)), ids(ItemSort.toAnswer(listOf(honest, rogue, older), emptyMap())))
    }

    @Test
    fun aFutureTimeNeverPutsAnItemBelowItsArrival() {
        val a = approval(1, now + 30 * day)
        val b = approval(2, now - hour)
        assertEquals(ids(listOf(b, a)), ids(ItemSort.toAnswer(listOf(a, b), emptyMap())))
    }

    @Test
    fun theClockOffsetsOfTwoPortalsAreApplied() {
        // Portal 1 runs 3 hours ahead: its item claims "now + 3 h - 1 h" and is 1 hour old. Portal 2 is right and 2 hours old.
        val ahead = approval(1, now + 2 * hour)
        val right = approval(2, now - 2 * hour)
        val offsets = mapOf(1L to 3 * hour, 2L to 0L)
        assertEquals(ids(listOf(right, ahead)), ids(ItemSort.toAnswer(listOf(ahead, right), offsets)))
        // A portal that runs behind: without the offset its item looks the oldest, with the offset it is the younger one.
        val behind = approval(3, now - 6 * hour)
        val behindOffsets = mapOf(3L to -5 * hour, 2L to 0L)
        // On the phone clock, `behind` was created at now - 1 h, `right` at now - 2 h.
        assertEquals(ids(listOf(right, behind)), ids(ItemSort.toAnswer(listOf(behind, right), behindOffsets)))
    }

    @Test
    fun aTieGoesToTheServerThenToTheRequestId() {
        val a = approval(2, now - hour)
        val b = approval(1, now - hour)
        val c = approval(1, now - hour)
        assertEquals(ids(listOf(b, c, a)), ids(ItemSort.toAnswer(listOf(a, c, b), emptyMap())))
    }

    @Test
    fun finalItemsAreNotToAnswer() {
        val open = approval(1, now - hour)
        val gone = approval(2, now - hour)
        engine.onSettled(2, Make.settled(gone.requestId, Make.hex(0x300), atMs = now))
        val all = listOf(open, repo.item(2, gone.requestId)!!)
        assertEquals(ids(listOf(open)), ids(ItemSort.toAnswer(all, emptyMap())))
        assertEquals(ids(listOf(gone)), ids(ItemSort.done(all, emptyMap())))
    }

    // ------------------------------------------------------------ the list "Done"

    @Test
    fun theDoneListShowsTheMostRecentEndFirstAcrossPortals() {
        val a = approval(1, now - 5 * hour)
        val b = approval(2, now - 5 * hour)
        arrival = now
        engine.onSettled(1, Make.settled(a.requestId, Make.hex(0x301), atMs = now - 3 * hour))
        engine.onSettled(2, Make.settled(b.requestId, Make.hex(0x302), atMs = now - hour))
        val all = listOf(repo.item(1, a.requestId)!!, repo.item(2, b.requestId)!!)
        assertEquals(ids(listOf(b, a)), ids(ItemSort.done(all, emptyMap())))
    }

    @Test
    fun aForgedEndTimeDoesNotPinAnItemOnTopOfTheDoneList() {
        val honest = approval(1, now - 5 * hour)
        val rogue = approval(2, now - 5 * hour)
        arrival = now
        engine.onSettled(1, Make.settled(honest.requestId, Make.hex(0x303), atMs = now - hour))
        engine.onSettled(2, Make.settled(rogue.requestId, Make.hex(0x304), atMs = now + 365 * day))
        val all = listOf(repo.item(1, honest.requestId)!!, repo.item(2, rogue.requestId)!!)
        // The forged end counts as the arrival of the frame ("now"): it is the most recent, but not more than "now".
        val done = ItemSort.done(all, emptyMap())
        assertEquals(now, ItemSort.doneKey(done.first(), emptyMap()))
        assertEquals(ids(listOf(rogue, honest)), ids(done))
        // A portal that forges 0 gets the lowest place: 7 days before the arrival.
        val zero = Make.settled(honest.requestId, Make.hex(0x305), atMs = 0)
        val item = repo.item(1, honest.requestId)!!.copy(settled = zero)
        assertEquals(now - 7 * day, ItemSort.doneKey(item, emptyMap()))
    }
}
