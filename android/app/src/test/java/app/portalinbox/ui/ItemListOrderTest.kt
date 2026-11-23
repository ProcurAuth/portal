package app.portalinbox.ui

import app.portalinbox.data.Item
import app.portalinbox.data.ItemEngine
import app.portalinbox.portal.ItemSort
import app.portalinbox.proto.ItemState
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ItemListOrderTest {
    private val server = 1L
    private val t0 = 1_788_000_000_000L
    private var now = t0 + 6 * 86_400_000L + 3 * 60_000L
    private val repo = MemoryItemRepository()
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { now })

    private fun approval(n: Int, createdAtMs: Long, serverId: Long = server): Item {
        engine.onRequest(serverId, Make.approval(Make.hex(0x10 + n), Make.hex(0x20 + n), createdAtMs = createdAtMs))
        return repo.item(serverId, Make.hex(0x10 + n))!!
    }

    private fun question(n: Int, createdAtMs: Long, file: String = "request-question-choice.json"): Item {
        engine.onRequest(server, Make.question(Make.hex(0x50 + n), Make.hex(0x60 + n), file = file, createdAtMs = createdAtMs))
        return repo.item(server, Make.hex(0x50 + n))!!
    }

    // ------------------------------------------------------------ alerts above the items

    @Test
    fun alertsComeAboveTheItemsToAnswer() {
        assertEquals(
            listOf(ItemListOrder.Section.ALERTS, ItemListOrder.Section.TO_ANSWER),
            ItemListOrder.sections(hasAlerts = true, hasItems = true),
        )
    }

    @Test
    fun theCardsOfTheNewAgentsSitBetweenTheAlertsAndTheItems() {
        assertEquals(
            listOf(ItemListOrder.Section.ALERTS, ItemListOrder.Section.NEW_AGENTS, ItemListOrder.Section.TO_ANSWER),
            ItemListOrder.sections(hasAlerts = true, hasItems = true, hasNewAgents = true),
        )
        // A new agent with no request yet still has its card, so the list is not "all clear".
        assertEquals(listOf(ItemListOrder.Section.NEW_AGENTS), ItemListOrder.sections(false, false, true))
        assertEquals(listOf(ItemListOrder.Section.ALERTS, ItemListOrder.Section.NEW_AGENTS), ItemListOrder.sections(true, false, true))
        assertTrue(ItemListOrder.sections(false, false, false).isEmpty())
    }

    @Test
    fun aSectionWithNoRowDoesNotShow() {
        assertEquals(listOf(ItemListOrder.Section.ALERTS), ItemListOrder.sections(true, false))
        assertEquals(listOf(ItemListOrder.Section.TO_ANSWER), ItemListOrder.sections(false, true))
        assertTrue(ItemListOrder.sections(false, false).isEmpty())
    }

    @Test
    fun theAlertsSectionIsAlwaysTheFirstOne() {
        for (alerts in listOf(true, false)) for (items in listOf(true, false)) {
            val s = ItemListOrder.sections(alerts, items)
            if (alerts) assertEquals(ItemListOrder.Section.ALERTS, s.first())
            if (items) assertEquals(ItemListOrder.Section.TO_ANSWER, s.last())
        }
    }

    // ------------------------------------------------------------ order inside "To answer"

    @Test
    fun theOldestRequestComesFirst() {
        val young = approval(1, t0 + 5 * 86_400_000L)
        val old = approval(2, t0)
        val mid = question(1, t0 + 86_400_000L)
        assertEquals(listOf(old.requestId, mid.requestId, young.requestId), ItemSort.toAnswer(listOf(young, mid, old), emptyMap()).map { it.requestId })
    }

    @Test
    fun aTieGoesToTheServerThenTheRequestId() {
        val a = approval(1, t0, serverId = 2)
        val b = approval(2, t0, serverId = 1)
        val c = approval(3, t0, serverId = 1)
        assertEquals(listOf(b, c, a).map { it.requestId }, ItemSort.toAnswer(listOf(a, c, b), emptyMap()).map { it.requestId })
    }

    @Test
    fun finalItemsAreNotToAnswer() {
        val open = approval(1, t0)
        val gone = approval(2, t0)
        engine.onSettled(server, Make.settled(gone.requestId, Make.hex(0x70)))
        val all = repo.items(server)
        assertEquals(listOf(open.requestId), ItemSort.toAnswer(all, emptyMap()).map { it.requestId })
        assertEquals(listOf(gone.requestId), ItemSort.done(all, emptyMap()).map { it.requestId })
    }

    @Test
    fun aRepliedItemIsStillToAnswerForTheOwnerView() {
        val i = approval(1, t0)
        engine.onMessage(server, Make.message(i.requestId, Make.hex(0x71), 1, user = true))
        assertEquals(ItemState.REPLIED, repo.item(server, i.requestId)!!.state)
        assertEquals(1, ItemSort.toAnswer(repo.items(server), emptyMap()).size)
    }

    @Test
    fun theDoneListShowsTheMostRecentEndFirst() {
        val a = approval(1, t0)
        val b = approval(2, t0)
        engine.onSettled(server, Make.settled(a.requestId, Make.hex(0x72), atMs = t0 + 1000))
        engine.onSettled(server, Make.settled(b.requestId, Make.hex(0x73), atMs = t0 + 9000))
        assertEquals(listOf(b.requestId, a.requestId), ItemSort.done(repo.items(server), emptyMap()).map { it.requestId })
    }

    // ------------------------------------------------------------ rows

    @Test
    fun anApprovalRowShowsTitleServiceAgesAndCode() {
        val r = ItemListOrder.row(approval(1, t0), now)
        assertEquals(ItemListOrder.Chip.APPROVAL, r.chip)
        assertEquals("ClickHouse write", r.title)
        assertEquals("Billing bot · datastore", r.subtitle)
        assertEquals("6 d", r.age)
        assertEquals("K7QF", r.code)
        assertEquals("Asked 6 d ago · agent active 6 d ago", r.detail)
        assertTrue(r.old)
        assertFalse(r.invalid)
        assertTrue(r.openable)
    }

    @Test
    fun aRecentApprovalIsNotOld() {
        now = t0 + 60_000
        assertFalse(ItemListOrder.row(approval(1, t0), now).old)
    }

    @Test
    fun anInvalidApprovalIsTitledInvalidRequestAndStaysInTheList() {
        engine.onRequest(server, Make.approval(Make.hex(0x99), Make.hex(0x98), effect = "sha256:" + "1".repeat(64)))
        val i = repo.item(server, Make.hex(0x99))!!
        val r = ItemListOrder.row(i, now)
        assertEquals("Invalid request", r.title)
        assertTrue(r.invalid)
        assertEquals(1, ItemSort.toAnswer(listOf(i), emptyMap()).size)
    }

    @Test
    fun aQuestionRowShowsTheKindOfAnswerAndOpensItsScreen() {
        val r = ItemListOrder.row(question(1, t0), now)
        assertEquals(ItemListOrder.Chip.QUESTION, r.chip)
        assertNull(r.code)
        assertTrue(r.openable)
        assertTrue(r.subtitle.contains("choices"))
        assertTrue(r.detail.startsWith("Agent active"))
    }

    @Test
    fun anAutomaticQuestionIsADecisionWithItsSuggestion() {
        val i = question(2, t0, file = "request-question-confirm-one-shot.json")
        val r = ItemListOrder.row(i, now)
        assertEquals(ItemListOrder.Chip.DECISION, r.chip)
        assertTrue(r.subtitle.startsWith("Automatic system · "))
    }

    @Test
    fun aHiddenCharacterInTheTitleShowsAsAMarkInTheRow() {
        val i = approval(1, t0)
        val v = i.view!!
        val r = ItemListOrder.row(i.copy(view = v.copy(title = "Run‮ it")), now)
        assertEquals("Run<U+202E> it", r.title)
    }

    @Test
    fun aDoneRowSaysWhatEndedTheItem() {
        val i = approval(1, t0)
        engine.onSettled(server, Make.settled(i.requestId, Make.hex(0x74), "settled-abandoned.json"))
        val row = ItemListOrder.doneRow(repo.item(server, i.requestId)!!)
        assertEquals("abandoned", row.word)
        assertEquals("The agent no longer answers", row.detail)
        val j = approval(2, t0)
        engine.onSettled(server, Make.settled(j.requestId, Make.hex(0x75), "settled-superseded.json"))
        assertEquals("replaced", ItemListOrder.doneRow(repo.item(server, j.requestId)!!).word)
    }
}
