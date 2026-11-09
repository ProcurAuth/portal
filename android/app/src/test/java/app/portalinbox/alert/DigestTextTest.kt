package app.portalinbox.alert

import app.portalinbox.alert.DigestText.Known
import app.portalinbox.data.ItemEvent
import app.portalinbox.proto.RequestKind
import app.portalinbox.testsupport.Fixtures
import app.portalinbox.testsupport.Make
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class DigestTextTest {
    private val day = 86_400_000L
    private val now = 1_788_000_000_000L

    @Test
    fun theTextHasTheCountAndTheAgeOfTheOldest() {
        val c = DigestText.of(3, now - 6 * day, listOf("a", "b", "c"), emptyMap(), now)!!
        assertEquals("3 items wait for you", c.title)
        assertEquals("The oldest is 6 days old.", c.text)
    }

    @Test
    fun oneItemUsesTheSingularForm() {
        val c = DigestText.of(1, now - 5 * 3_600_000L, listOf("a"), emptyMap(), now)!!
        assertEquals("1 item waits for you", c.title)
        assertEquals("The oldest is 5 hours old.", c.text)
    }

    @Test
    fun thePhoneAddsTheKindsItKnowsForEachListedId() {
        val known = mapOf(
            "a" to Known(RequestKind.APPROVAL, true, now - 6 * day),
            "b" to Known(RequestKind.QUESTION, true, now - day),
        )
        val c = DigestText.of(2, now - 6 * day, listOf("a", "b"), known, now)!!
        assertEquals("2 items wait for you", c.title)
        assertEquals("1 approval and 1 question. The oldest is 6 days old.", c.text)
    }

    @Test
    fun theLockScreenVersionHasNoKindsOnlyTheAge() {
        val known = mapOf("a" to Known(RequestKind.APPROVAL, true, now - 6 * day), "b" to Known(RequestKind.QUESTION, true, now - day))
        val c = DigestText.of(2, now - 6 * day, listOf("a", "b"), known, now)!!
        assertEquals("2 items wait for you", c.publicTitle)
        assertEquals("The oldest is 6 days old.", c.publicText)
    }

    @Test
    fun theKindsAreLeftOutWhenTheListIsNotComplete() {
        // 3 items wait, the frame names 2 of them, or the phone does not know one.
        assertEquals("The oldest is 2 days old.", DigestText.of(3, now - 2 * day, listOf("a", "b"), mapOf("a" to Known(RequestKind.APPROVAL, true, now), "b" to Known(RequestKind.APPROVAL, true, now)), now)!!.text)
        assertEquals("The oldest is 2 days old.", DigestText.of(2, now - 2 * day, listOf("a", "b"), mapOf("a" to Known(RequestKind.APPROVAL, true, now)), now)!!.text)
    }

    @Test
    fun anItemTheOwnerClosedLeavesTheCount() {
        val known = mapOf(
            "a" to Known(RequestKind.APPROVAL, false, now - 6 * day),
            "b" to Known(RequestKind.QUESTION, true, now - day),
        )
        val c = DigestText.of(2, now - 6 * day, listOf("a", "b"), known, now)!!
        assertEquals("1 item waits for you", c.title)
        // The oldest open item is the one of the day before.
        assertEquals("1 question. The oldest is 1 day old.", c.text)
    }

    @Test
    fun aDigestOfItemsThatAreAllClosedMakesNoNotification() {
        val known = mapOf("a" to Known(RequestKind.APPROVAL, false, now - day), "b" to Known(RequestKind.QUESTION, false, now - day))
        assertNull(DigestText.of(2, now - day, listOf("a", "b"), known, now))
    }

    @Test
    fun aClosedItemIsDroppedEvenWhenTheListIsIncomplete() {
        val known = mapOf("a" to Known(RequestKind.APPROVAL, false, now - day))
        val c = DigestText.of(5, now - 3 * day, listOf("a", "b"), known, now)!!
        assertEquals("4 items wait for you", c.title)
    }

    @Test
    fun noTextHoldsATitleACommandOrAnId() {
        val known = mapOf("a" to Known(RequestKind.APPROVAL, true, now - day))
        val c = DigestText.of(1, now - day, listOf("a"), known, now)!!
        for (t in listOf(c.title, c.text, c.publicTitle, c.publicText)) {
            assertFalse(t, t.contains("INSERT"))
            assertFalse(t, t.contains("K7QF"))
            assertFalse(t, t.contains("a,"))
        }
    }

    @Test
    fun theBreakdownNamesApprovalsAndQuestions() {
        assertEquals("2 approvals", DigestText.breakdown(listOf(RequestKind.APPROVAL, RequestKind.APPROVAL)))
        assertEquals("1 approval and 2 questions", DigestText.breakdown(listOf(RequestKind.APPROVAL, RequestKind.QUESTION, RequestKind.QUESTION)))
        assertNull(DigestText.breakdown(emptyList()))
    }

    @Test
    fun theGoldenDigestFrameMakesTheNotification() {
        val f = Make.digest(Make.hex(7))
        val e = ItemEvent.Digest(1, f.count, f.oldestCreatedAtMs, f.ids)
        val c = DigestText.of(e.count, e.oldestCreatedAtMs, e.ids, emptyMap(), 1_787_400_000_000L + 6 * day)!!
        assertEquals("3 items wait for you", c.title)
        assertTrue(c.text.startsWith("The oldest is 6 days"))
        assertEquals(3L, Fixtures.golden("server", "digest.json").json.getLong("count"))
    }
}
