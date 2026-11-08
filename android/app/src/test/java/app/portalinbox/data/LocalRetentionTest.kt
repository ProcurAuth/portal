package app.portalinbox.data

import app.portalinbox.proto.ItemState
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class LocalRetentionTest {
    private val server = 1L
    private val day = 86_400_000L
    private var now = 1_800_000_000_000L
    private val repo = MemoryItemRepository()
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { now })

    private fun open(n: Int): String {
        val id = Make.hex(0x100 + n)
        engine.onRequest(server, Make.approval(id, Make.hex(0x200 + n), createdAtMs = now))
        return id
    }

    private fun settle(id: String, n: Int, atMs: Long = now) {
        engine.onSettled(server, Make.settled(id, Make.hex(0x300 + n), "settled-answered.json", atMs))
    }

    @Test
    fun theDefaultIs30DaysAndTheOptionsAreFixed() {
        assertEquals(30, LocalRetention.DEFAULT_DAYS)
        assertEquals(listOf(7, 30, 90, 180, 365), LocalRetention.OPTIONS_DAYS)
        assertEquals(30, LocalRetention.daysOrDefault(null))
        assertEquals(30, LocalRetention.daysOrDefault(12))
        assertEquals(90, LocalRetention.daysOrDefault(90))
    }

    @Test
    fun aFinalItemOlderThanTheRetentionIsErased() {
        val id = open(1)
        settle(id, 1, now)
        now += 31 * day
        assertEquals(1, LocalRetention.purge(repo, server, now, 30))
        assertNull(repo.item(server, id))
    }

    @Test
    fun aFinalItemInsideTheRetentionStays() {
        val id = open(1)
        settle(id, 1, now)
        now += 29 * day
        assertEquals(0, LocalRetention.purge(repo, server, now, 30))
        assertNotNull(repo.item(server, id))
    }

    @Test
    fun theLimitIsStrict() {
        val id = open(1)
        settle(id, 1, now)
        val end = now
        assertFalse(LocalRetention.expired(repo.item(server, id)!!, end + 30 * day, 30))
        assertTrue(LocalRetention.expired(repo.item(server, id)!!, end + 30 * day + 1, 30))
    }

    @Test
    fun anOpenItemIsNeverErasedWhateverItsAge() {
        val id = open(1)
        now += 400 * day
        assertEquals(0, LocalRetention.purge(repo, server, now, 7))
        assertNotNull(repo.item(server, id))
    }

    @Test
    fun theEndIsTheTimeOfTheSettledFrame() {
        val id = open(1)
        val longAgo = now - 100 * day
        settle(id, 1, longAgo)
        assertEquals(longAgo, LocalRetention.endOf(repo.item(server, id)!!))
        assertEquals(1, LocalRetention.purge(repo, server, now, 30))
    }

    @Test
    fun theEraseTakesTheTurnsAndTheOutboxRowsOfTheItem() {
        val id = open(1)
        engine.onMessage(server, Make.message(id, Make.hex(0x400), 1, user = false))
        engine.approve(server, id)
        settle(id, 1, now)
        assertTrue(repo.turns(server, id).isNotEmpty())
        assertTrue(repo.outboxOf(server, id).isNotEmpty())
        now += 40 * day
        LocalRetention.purge(repo, server, now, 30)
        assertTrue(repo.turns(server, id).isEmpty())
        assertTrue(repo.outboxOf(server, id).isEmpty())
    }

    @Test
    fun onlyTheExpiredItemsGoAndOthersStay() {
        val a = open(1)
        settle(a, 1, now)
        now += 20 * day
        val b = open(2)
        settle(b, 2, now)
        now += 15 * day
        assertEquals(1, LocalRetention.purge(repo, server, now, 30))
        assertNull(repo.item(server, a))
        assertNotNull(repo.item(server, b))
    }

    @Test
    fun oldDeliveryRowsGoToo() {
        open(1)
        val before = repo.deliveryCount(server)
        assertTrue(before > 0)
        now += 40 * day
        LocalRetention.purge(repo, server, now, 30)
        assertEquals(0, repo.deliveryCount(server))
    }

    @Test
    fun anotherServerIsNotTouched() {
        val id = open(1)
        settle(id, 1, now)
        now += 40 * day
        assertEquals(0, LocalRetention.purge(repo, 2L, now, 30))
        assertNotNull(repo.item(server, id))
    }

    @Test
    fun aRequestThatComesAgainAfterTheEraseIsANewItem() {
        val id = open(1)
        settle(id, 1, now)
        now += 40 * day
        LocalRetention.purge(repo, server, now, 30)
        val out = engine.onRequest(server, Make.approval(id, Make.hex(0x900), createdAtMs = now))
        assertEquals(ItemState.PENDING, repo.item(server, id)!!.state)
        assertEquals(1, out.events.size)
    }

    @Test
    fun theLabelsAreShort() {
        assertEquals("7 days", LocalRetention.label(7))
        assertEquals("6 months", LocalRetention.label(180))
        assertEquals("1 year", LocalRetention.label(365))
    }
}
