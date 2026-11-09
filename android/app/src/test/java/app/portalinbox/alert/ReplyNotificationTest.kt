package app.portalinbox.alert

import app.portalinbox.data.Item
import app.portalinbox.data.ItemEngine
import app.portalinbox.data.ItemEvent
import app.portalinbox.proto.ItemState
import app.portalinbox.proto.QuietPrefs
import app.portalinbox.proto.Urgency
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ReplyNotificationTest {
    private val server = 1L
    private val repo = MemoryItemRepository()
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { 1_788_000_100_000L })
    private val rid = Make.hex(0xb)

    private fun approval(edit: (org.json.JSONObject) -> Unit = {}): Item {
        engine.onRequest(server, Make.approval(rid, Make.hex(1), edit = edit))
        return repo.item(server, rid)!!
    }

    private fun agentMessage(): ItemEvent.AgentMessage =
        engine.onMessage(server, Make.message(rid, Make.hex(5), 1, user = false)).events.single() as ItemEvent.AgentMessage

    // ------------------------------------------------------------ a turn of the agent

    @Test
    fun aTurnOfTheAgentNotifiesOnTheRepliesChannel() {
        val item = approval()
        val plan = NotificationPlan.forMessage(agentMessage(), item)!!
        assertEquals(NotificationPlan.CH_REPLIES, plan.channel)
        assertEquals(NotificationPlan.ID_REPLY, plan.notificationId)
        assertEquals(NotificationPlan.Target.THREAD, plan.target)
        assertFalse(plan.declineAction)
        assertFalse(plan.ring)
        assertEquals(rid, plan.tag)
        assertEquals("Reply from claude-laptop", plan.content.title)
        assertEquals("Approval K7QF", plan.content.text)
    }

    @Test
    fun theNotificationNeverHoldsTheTextOfTheTurn() {
        val item = approval()
        val plan = NotificationPlan.forMessage(agentMessage(), item)!!
        val all = listOf(plan.content.title, plan.content.text, plan.content.publicTitle, plan.content.publicText)
        for (secret in listOf("The table has 3 rows", "SELECT", "analytics", "INSERT")) {
            for (t in all) assertFalse("'$secret' in '$t'", t.contains(secret))
        }
        assertEquals("Reply from an agent", plan.content.publicTitle)
        assertEquals("Content hidden. Unlock to read.", plan.content.publicText)
    }

    @Test
    fun aReplyToAQuestionNamesTheQuestionTitle() {
        engine.onRequest(server, Make.question(rid, Make.hex(2)))
        val item = repo.item(server, rid)!!
        val plan = NotificationPlan.forMessage(agentMessage(), item)!!
        assertEquals("Purge the staging data of November?", plan.content.text)
    }

    @Test
    fun aTurnOfTheOwnerMakesNoNotification() {
        val item = approval()
        val user = engine.onMessage(server, Make.message(rid, Make.hex(6), 1, user = true)).events.single()
        assertTrue(user is ItemEvent.UserMessage)
        assertNull(NotificationPlan.forMessage(user, item))
    }

    @Test
    fun aTurnForAClosedOrMissingItemMakesNoNotification() {
        val item = approval()
        val msg = agentMessage()
        assertNull(NotificationPlan.forMessage(msg, null))
        assertNull(NotificationPlan.forMessage(msg, item.copy(state = ItemState.ABANDONED)))
    }

    @Test
    fun aTurnInQuietHoursUsesTheSilentRepliesChannel() {
        val item = approval { it.put("urgency", Urgency.NORMAL.wire) }
        val night = QuietPrefs("00:00", "23:59", "UTC", highRings = false)
        val plan = NotificationPlan.forMessage(agentMessage(), item, NotificationPlan.Env(night, 1_788_000_000_000L))!!
        assertEquals(NotificationPlan.CH_REPLIES_QUIET, plan.channel)
        assertEquals("replies_quiet", NotificationPlan.CH_REPLIES_QUIET)
    }

    // ------------------------------------------------------------ the agent is back

    private fun abandonedAndBack(notified: Boolean): ItemEvent.Revived {
        approval()
        if (notified) repo.markNotified(server, rid)
        engine.onSettled(server, Make.settled(rid, Make.hex(8), "settled-abandoned.json"))
        val out = engine.onRequest(server, Make.approval(rid, Make.hex(9), seq = 1))
        return out.events.single() as ItemEvent.Revived
    }

    @Test
    fun theReturnOfTheAgentMakesOneNotificationThatSaysSo() {
        val plan = NotificationPlan.forEvent(abandonedAndBack(notified = true))!!
        assertEquals("The agent is back", plan.content.title)
        assertEquals("claude-laptop answers again: K7QF", plan.content.text)
        assertEquals(NotificationPlan.CH_REPLIES, plan.channel)
        assertFalse(plan.ring)
        assertFalse(plan.declineAction)
        assertFalse(plan.refresh)
        assertEquals(NotificationPlan.Target.APPROVAL, plan.target)
        assertEquals("Content hidden. Unlock to review.", plan.content.publicText)
    }

    @Test
    fun theReturnOfTheAgentInQuietHoursHasNoSound() {
        val event = abandonedAndBack(notified = true)
        val night = QuietPrefs("00:00", "23:59", "UTC", highRings = false)
        val plan = NotificationPlan.forEvent(event, NotificationPlan.Env(night, 1_788_000_000_000L))!!
        assertEquals(NotificationPlan.CH_REPLIES_QUIET, plan.channel)
    }

    @Test
    fun aReturnOfAnItemThePhoneNeverShowedNotifiesLikeANewRequest() {
        val plan = NotificationPlan.forEvent(abandonedAndBack(notified = false))!!
        assertEquals(NotificationPlan.CH_REQUESTS, plan.channel)
        assertTrue(plan.declineAction)
    }

    @Test
    fun theReturnNotificationTakesTheTagOfTheChain() {
        val event = abandonedAndBack(notified = true)
        assertEquals(event.item.request.chainId, NotificationPlan.forEvent(event)!!.tag)
        assertEquals(NotificationPlan.ID_REQUEST, NotificationPlan.forEvent(event)!!.notificationId)
    }

    @Test
    fun theIdsOfTheNotificationsAreDistinct() {
        assertEquals(3, setOf(NotificationPlan.ID_REQUEST, NotificationPlan.ID_DIGEST, NotificationPlan.ID_REPLY).size)
    }

    @Test
    fun aQuestionNotificationKeepsItsOpenAction() {
        engine.onRequest(server, Make.question(rid, Make.hex(2)))
        val plan = NotificationPlan.forEvent(ItemEvent.NewRequest(repo.item(server, rid)!!))!!
        assertTrue(plan.openAction)
        assertFalse(plan.declineAction)
        engine.onRequest(server, Make.approval(Make.hex(0xc), Make.hex(3)))
        assertFalse(NotificationPlan.forEvent(ItemEvent.NewRequest(repo.item(server, Make.hex(0xc))!!))!!.openAction)
    }
}
