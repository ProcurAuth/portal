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

    // ------------------------------------------------------------ the own value of a portal

    @Test
    fun aPortalFollowsTheGlobalValueUnlessItHasItsOwn() {
        assertEquals(30, LocalRetention.daysFor(30, null))
        assertEquals(7, LocalRetention.daysFor(30, 7))
        assertEquals(365, LocalRetention.daysFor(30, 365))
        // The own value can be longer or shorter than the global one.
        assertEquals(90, LocalRetention.daysFor(7, 90))
    }

    @Test
    fun anOwnValueThatIsNotAnOptionIsNotReadAndTheGlobalValueApplies() {
        assertEquals(30, LocalRetention.daysFor(30, 12))
        assertEquals(90, LocalRetention.daysFor(90, 0))
        assertEquals(90, LocalRetention.daysFor(90, -5))
        // A global value that is not an option gives the default, as before.
        assertEquals(30, LocalRetention.daysFor(12, null))
    }

    @Test
    fun onlyNoValueOrAnOptionCanBeStoredAsTheOwnValue() {
        assertTrue(LocalRetention.validOverride(null))
        for (d in LocalRetention.OPTIONS_DAYS) assertTrue(LocalRetention.validOverride(d))
        assertFalse(LocalRetention.validOverride(12))
        assertFalse(LocalRetention.validOverride(0))
    }

    @Test
    fun theChoicesAreSameAsAllThenTheOptions() {
        assertEquals(listOf(null, 7, 30, 90, 180, 365), LocalRetention.overrideChoices())
        assertEquals("Same as all (30 days)", LocalRetention.overrideLabel(null, 30))
        assertEquals("Same as all (1 year)", LocalRetention.overrideLabel(null, 365))
        assertEquals("90 days", LocalRetention.overrideLabel(90, 30))
        assertEquals("1 year", LocalRetention.overrideLabel(365, 30))
        assertEquals("Same as all (30 days)", LocalRetention.overrideLabel(12, 30))
    }

    @Test
    fun eachPortalErasesAtItsOwnAgeAndOneOverrideDoesNotTouchTheOther() {
        val a = open(1)
        settle(a, 1, now)
        val b = open(2)
        engine.onRequest(2L, Make.approval(Make.hex(0x700), Make.hex(0x701), createdAtMs = now))
        engine.onSettled(2L, Make.settled(Make.hex(0x700), Make.hex(0x702), "settled-answered.json", now))
        now += 20 * day
        // Portal 1 keeps 7 days, portal 2 follows the global 30.
        assertEquals(1, LocalRetention.purge(repo, 1L, now, LocalRetention.daysFor(30, 7)))
        assertEquals(0, LocalRetention.purge(repo, 2L, now, LocalRetention.daysFor(30, null)))
        assertNull(repo.item(1L, a))
        assertNotNull(repo.item(2L, Make.hex(0x700)))
        assertNotNull(b)
    }

    @Test
    fun theCountOfWhatAShorterValueWouldEraseChangesNothing() {
        val a = open(1)
        settle(a, 1, now)
        val b = open(2)
        now += 10 * day
        settle(b, 2, now)
        now += 10 * day
        // At 20 and 10 days of age, 7 days would erase both, 30 none.
        assertEquals(2, LocalRetention.wouldErase(repo, server, now, 7))
        assertEquals(0, LocalRetention.wouldErase(repo, server, now, 30))
        assertNotNull(repo.item(server, a))
        assertNotNull(repo.item(server, b))
    }
}
