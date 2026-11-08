package app.portalinbox.alert

import app.portalinbox.data.Item
import app.portalinbox.data.ItemEngine
import app.portalinbox.data.ItemEvent
import app.portalinbox.proto.Urgency
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class NotificationTextTest {
    private val server = 1L
    private val repo = MemoryItemRepository()
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { 1_788_000_100_000L })
    private val rid = Make.hex(0xb)

    private fun approval(id: String = rid, delivery: Int = 1, edit: (org.json.JSONObject) -> Unit = {}): Item {
        engine.onRequest(server, Make.approval(id, Make.hex(delivery), edit = edit))
        return repo.item(server, id)!!
    }

    @Test
    fun theApprovalTextNamesTheCodeTheKeyAndTheService() {
        val c = NotificationText.approval(approval())
        assertEquals("Approval requested · K7QF", c.title)
        assertEquals("key claude-laptop · datastore", c.text)
    }

    @Test
    fun thePublicVersionNamesTheCodeOnly() {
        val c = NotificationText.approval(approval())
        assertEquals("Approval K7QF", c.publicTitle)
        assertEquals("Content hidden. Unlock to review.", c.publicText)
    }

    @Test
    fun noTextOfAnApprovalHoldsTheCommandTheViewOrTheExplanation() {
        val i = approval()
        val c = NotificationText.approval(i)
        val all = listOf(c.title, c.text, c.publicTitle, c.publicText)
        // The command, a field of the operation, the title of the view, the label of a field, the explanation and the hint.
        for (secret in listOf("INSERT", "analytics", "chdash_mcp_write", "ClickHouse write", "Database", "migration", "dry run", "approve", "SELECT")) {
            for (t in all) assertFalse("'$secret' in '$t'", t.contains(secret, ignoreCase = true))
        }
        // The effect hash and the resource do not show either.
        for (t in all) {
            assertFalse(t.contains("sha256"))
            assertFalse(t.contains("host:prod"))
        }
    }

    @Test
    fun aHiddenCharacterInTheKeyNameShowsAsAMark() {
        val i = approval { it.getJSONObject("from").put("key_name", "ops‮agent") }
        assertEquals("key ops<U+202E>agent · datastore", NotificationText.approval(i).text)
    }

    @Test
    fun theQuestionTextNamesTheTitleAndTheSenderButHasNoBody() {
        engine.onRequest(server, Make.question(Make.hex(0xc), Make.hex(2)))
        val i = repo.item(server, Make.hex(0xc))!!
        val c = NotificationText.question(i)
        assertEquals("Question from an agent", c.title)
        assertEquals("Question from an agent", c.publicTitle)
        assertEquals("Content hidden. Unlock to read.", c.publicText)
        assertFalse(c.text.contains("DELETE"))
        assertFalse(c.publicText.contains(i.request.from.label))
        assertTrue(c.text.endsWith(i.request.from.label))
    }

    @Test
    fun aNewApprovalRingsOnTheRequestChannelWithTheDeclineActionOnly() {
        val i = approval()
        val plan = NotificationPlan.forEvent(ItemEvent.NewRequest(i))!!
        assertEquals(NotificationPlan.CH_REQUESTS, plan.channel)
        assertTrue(plan.ring)
        assertTrue(plan.declineAction)
        assertFalse(plan.refresh)
        assertEquals(i.request.chainId, plan.tag)
    }

    @Test
    fun aQuietRequestUsesTheQuietChannelWithNoSound() {
        val i = approval { it.put("quiet", true) }
        val plan = NotificationPlan.forEvent(ItemEvent.NewRequest(i))!!
        assertEquals(NotificationPlan.CH_REQUESTS_QUIET, plan.channel)
        assertFalse(plan.ring)
    }

    @Test
    fun aLowUrgencyRequestIsQuietToo() {
        val i = approval { it.put("urgency", Urgency.LOW.wire) }
        assertFalse(NotificationPlan.forEvent(ItemEvent.NewRequest(i))!!.ring)
    }

    @Test
    fun aQuestionHasNoDeclineAction() {
        engine.onRequest(server, Make.question(Make.hex(0xc), Make.hex(2)))
        val plan = NotificationPlan.forEvent(ItemEvent.NewRequest(repo.item(server, Make.hex(0xc))!!))!!
        assertFalse(plan.declineAction)
    }

    @Test
    fun anUpdateOfANotifiedItemRefreshesWithoutSound() {
        val i = approval()
        repo.markNotified(server, rid)
        val notified = repo.item(server, rid)!!
        val plan = NotificationPlan.forEvent(ItemEvent.Updated(notified))!!
        assertTrue(plan.refresh)
        assertFalse(plan.ring)
        // An update of an item that never made a notification notifies like a new one.
        val fresh = NotificationPlan.forEvent(ItemEvent.Updated(i))!!
        assertFalse(fresh.refresh)
        assertTrue(fresh.ring)
    }

    @Test
    fun otherEventsMakeNoApprovalNotification() {
        val i = approval()
        assertNull(NotificationPlan.forEvent(ItemEvent.Settled(i)))
        assertNull(NotificationPlan.forEvent(ItemEvent.Digest(server, 2, 0, emptyList())))
    }

    @Test
    fun aSettledItemWithdrawsTheTagOfItsChain() {
        val i = approval()
        assertEquals(i.request.chainId, NotificationPlan.withdrawTag(i, emptyList()))
    }

    @Test
    fun aNewerRequestOfTheSameChainKeepsTheNotification() {
        val old = approval()
        val newer = approval(id = Make.hex(0xe), delivery = 3) { it.put("chain_id", rid) }
        assertEquals(old.request.chainId, newer.request.chainId)
        // The old request is settled (superseded) while the newer one is open: the tag stays.
        assertNull(NotificationPlan.withdrawTag(old, listOf(newer)))
        // An open item of another chain does not hold the tag.
        val other = approval(id = Make.hex(0xf), delivery = 4)
        assertEquals(old.request.chainId, NotificationPlan.withdrawTag(old, listOf(other)))
    }

    @Test
    fun theChannelIdsAreStable() {
        assertEquals("approvals", NotificationPlan.CH_REQUESTS)
        assertEquals("approvals_quiet", NotificationPlan.CH_REQUESTS_QUIET)
        assertEquals("digest", NotificationPlan.CH_DIGEST)
        assertEquals("replies", NotificationPlan.CH_REPLIES)
    }
}
